//! WebSocket 双向隧道：客户端 WebSocket ↔ BFF ↔ 上游 WebSocket。
//!
//! 原理：
//! - BFF 收到客户端 WS 升级请求后，对上游发起独立的 WS 连接
//! - 两个 spawn task 分别处理 客户端→上游 和 上游→客户端 两个方向
//! - 任一方向断开即终止整个隧道
//!
//! 生产加固：
//! - 上游握手带 **connect 超时**（黑洞上游不再让升级请求永久悬挂）；
//! - 空闲超时 + 双向心跳（Ping），防连接泄漏与被中间设备静默回收；
//! - 消息大小上限（超限以 1009 关闭）；
//! - 面向上游握手注入 `Authorization: Bearer <token>`（浏览器无法在 WS 握手中
//!   携带自定义头，BFF 在服务端侧补上，使 `auth_required` 的 WS 路由真正可用）。
//!
//! 优势（vs TCP 隧道）：
//! - 可在应用层注入认证、日志、指标
//! - 与现有熔断器、限流器兼容

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite;

/// WS 隧道运行参数（来自配置 `websocket` 段）。
#[derive(Debug, Clone)]
pub struct TunnelConfig {
    /// 上游握手连接超时
    pub connect_timeout: Duration,
    /// 空闲超时（双向均无消息超过该时长则关闭；0 = 禁用）
    pub idle_timeout: Duration,
    /// 心跳间隔（周期向对端发 Ping；0 = 禁用）
    pub heartbeat_interval: Duration,
    /// 单条消息最大字节数
    pub max_message_bytes: usize,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(300),
            heartbeat_interval: Duration::from_secs(30),
            max_message_bytes: 1024 * 1024,
        }
    }
}

/// WebSocket 双向隧道：客户端 ↔ 上游。
///
/// # Arguments
/// * `client_ws` - Axum 升级后的客户端 WebSocket
/// * `upstream_url` - 上游 WS URL（ws:// 或 wss:// 协议）
/// * `auth_token` - 可选认证令牌：以 `Authorization: Bearer` 注入上游握手
/// * `cfg` - 超时/心跳/大小上限参数
pub async fn ws_tunnel(
    mut client_ws: WebSocket,
    upstream_url: String,
    auth_token: Option<String>,
    cfg: TunnelConfig,
) {
    tracing::info!(%upstream_url, "WebSocket 隧道建立中");

    // 1. 连接上游 WebSocket（带 connect 超时与 Bearer 注入）
    let connect = connect_upstream(&upstream_url, auth_token.as_deref());
    let upstream_ws = match tokio::time::timeout(cfg.connect_timeout, connect).await {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(e)) => {
            let reason = format!("上游 WebSocket 连接失败: {} (url={})", e, upstream_url);
            tracing::error!(%upstream_url, %e, "WebSocket 隧道建立失败 — 请检查: 1) 上游服务是否在目标端口运行 2) 端点路径是否正确 3) strip_prefix 配置是否匹配");
            let _ = client_ws
                .send(Message::Close(Some(CloseFrame {
                    code: 1011,
                    reason: reason.into(),
                })))
                .await;
            return;
        }
        Err(_) => {
            let reason = format!(
                "上游 WebSocket 握手超时（{}s）(url={})",
                cfg.connect_timeout.as_secs(),
                upstream_url
            );
            tracing::error!(%upstream_url, "WebSocket 隧道建立超时");
            let _ = client_ws
                .send(Message::Close(Some(CloseFrame {
                    code: 1011,
                    reason: reason.into(),
                })))
                .await;
            return;
        }
    };

    tracing::info!(%upstream_url, "WebSocket 隧道已建立，开始双向 relay");

    let (mut upstream_sink, mut upstream_stream) = upstream_ws.split();
    let (mut client_sink, mut client_stream) = client_ws.split();

    let max_msg = cfg.max_message_bytes;
    let idle = cfg.idle_timeout;
    let heartbeat = cfg.heartbeat_interval;

    // 2. 客户端 → 上游（含心跳 + 空闲超时 + 大小上限）
    let mut c2u = tokio::spawn(async move {
        let mut hb = interval_opt(heartbeat);
        loop {
            tokio::select! {
                _ = tick_opt(&mut hb) => {
                    if upstream_sink.send(tungstenite::Message::Ping(Vec::new())).await.is_err() {
                        break;
                    }
                }
                read = timeout_opt(idle, client_stream.next()) => {
                    match read {
                        Ok(Some(Ok(msg))) => {
                            if message_too_big_axum(&msg, max_msg) {
                                let _ = upstream_sink.send(tungstenite::Message::Close(Some(
                                    tungstenite::protocol::CloseFrame {
                                        code: tungstenite::protocol::frame::coding::CloseCode::Size,
                                        reason: "消息超过大小上限".into(),
                                    },
                                ))).await;
                                break;
                            }
                            let upstream_msg = match msg {
                                Message::Text(t) => tungstenite::Message::Text(t.to_string()),
                                Message::Binary(b) => tungstenite::Message::Binary(b.to_vec()),
                                Message::Ping(d) => tungstenite::Message::Ping(d.to_vec()),
                                Message::Pong(d) => tungstenite::Message::Pong(d.to_vec()),
                                Message::Close(c) => {
                                    let frame = c.map(|f| tungstenite::protocol::CloseFrame {
                                        code: tungstenite::protocol::frame::coding::CloseCode::from(f.code),
                                        reason: f.reason.to_string().into(),
                                    });
                                    let _ = upstream_sink.send(tungstenite::Message::Close(frame)).await;
                                    break;
                                }
                            };
                            if upstream_sink.send(upstream_msg).await.is_err() {
                                tracing::warn!("上游 WS sink 已关闭");
                                break;
                            }
                        }
                        Ok(Some(Err(e))) => {
                            tracing::warn!("客户端 WS 读取错误: {}", e);
                            break;
                        }
                        Ok(None) => break,
                        Err(_) => {
                            tracing::info!("客户端 WS 空闲超时，关闭隧道");
                            break;
                        }
                    }
                }
            }
        }
    });

    // 3. 上游 → 客户端（含心跳 + 空闲超时 + 大小上限）
    let mut u2c = tokio::spawn(async move {
        let mut hb = interval_opt(heartbeat);
        loop {
            tokio::select! {
                _ = tick_opt(&mut hb) => {
                    if client_sink.send(Message::Ping(Default::default())).await.is_err() {
                        break;
                    }
                }
                read = timeout_opt(idle, upstream_stream.next()) => {
                    match read {
                        Ok(Some(Ok(msg))) => {
                            if message_too_big(&msg, max_msg) {
                                let _ = client_sink.send(Message::Close(Some(CloseFrame {
                                    code: 1009,
                                    reason: "消息超过大小上限".into(),
                                }))).await;
                                break;
                            }
                            let client_msg = match msg {
                                tungstenite::Message::Text(t) => Message::Text(t.into()),
                                tungstenite::Message::Binary(b) => Message::Binary(b.into()),
                                tungstenite::Message::Ping(d) => Message::Ping(d.into()),
                                tungstenite::Message::Pong(d) => Message::Pong(d.into()),
                                tungstenite::Message::Close(c) => {
                                    let frame = c.map(|f| CloseFrame {
                                        code: f.code.into(),
                                        reason: f.reason.to_string().into(),
                                    });
                                    let _ = client_sink.send(Message::Close(frame)).await;
                                    break;
                                }
                                tungstenite::Message::Frame(_) => continue,
                            };
                            if client_sink.send(client_msg).await.is_err() {
                                tracing::warn!("客户端 WS sink 已关闭");
                                break;
                            }
                        }
                        Ok(Some(Err(e))) => {
                            tracing::warn!("上游 WS 流读取错误: {}", e);
                            break;
                        }
                        Ok(None) => break,
                        Err(_) => {
                            tracing::info!("上游 WS 空闲超时，关闭隧道");
                            break;
                        }
                    }
                }
            }
        }
    });

    // 4. 任一方向断开即终止（同时 abort 另一方向，避免半连接泄漏）
    tokio::select! {
        _ = &mut c2u => {
            tracing::info!("客户端→上游 方向断开");
            u2c.abort();
        }
        _ = &mut u2c => {
            tracing::info!("上游→客户端 方向断开");
            c2u.abort();
        }
    }
    tracing::info!("WebSocket 隧道已关闭");
}

/// 连接上游：可选注入 Bearer 令牌（服务端侧握手支持自定义头）。
///
/// clippy：`Err` 变体（含 http::Response）较大 → 装箱。（async fn 无法直接加
/// allow 属性作用于返回类型，故用 Box。）
#[allow(clippy::result_large_err)]
async fn connect_upstream(
    url: &str,
    token: Option<&str>,
) -> Result<
    (
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>,
    ),
    tokio_tungstenite::tungstenite::Error,
> {
    match token {
        Some(t) => {
            use tokio_tungstenite::tungstenite::client::IntoClientRequest;
            let mut request = url.into_client_request()?;
            if let Ok(val) = tokio_tungstenite::tungstenite::http::HeaderValue::from_str(&format!(
                "Bearer {}",
                t
            )) {
                request.headers_mut().insert(
                    tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
                    val,
                );
            }
            tokio_tungstenite::connect_async(request).await
        }
        None => tokio_tungstenite::connect_async(url).await,
    }
}

/// 判断消息是否超过大小上限（上游 tungstenite 消息）。
fn message_too_big(msg: &tungstenite::Message, max: usize) -> bool {
    match msg {
        tungstenite::Message::Text(t) => t.len() > max,
        tungstenite::Message::Binary(b) => b.len() > max,
        _ => false,
    }
}

/// 判断消息是否超过大小上限（客户端 axum 消息）。
fn message_too_big_axum(msg: &Message, max: usize) -> bool {
    match msg {
        Message::Text(t) => t.len() > max,
        Message::Binary(b) => b.len() > max,
        _ => false,
    }
}

/// 构造可选心跳定时器（间隔为 0 时返回 None）。
fn interval_opt(period: Duration) -> Option<tokio::time::Interval> {
    if period == Duration::ZERO {
        None
    } else {
        let i = tokio::time::interval(period);
        Some(i)
    }
}

/// 等待定时器 tick；无定时器时挂起（永不就绪）。
async fn tick_opt(interval: &mut Option<tokio::time::Interval>) {
    match interval {
        Some(i) => {
            i.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// 空闲超时包装；0 时不做超时。
async fn timeout_opt<F: std::future::Future>(timeout: Duration, fut: F) -> Result<F::Output, ()> {
    if timeout == Duration::ZERO {
        Ok(fut.await)
    } else {
        tokio::time::timeout(timeout, fut).await.map_err(|_| ())
    }
}
