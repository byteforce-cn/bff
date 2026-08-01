//! WebSocket 双向隧道：客户端 WebSocket ↔ BFF ↔ 上游 WebSocket。
//!
//! 原理：
//! - BFF 收到客户端 WS 升级请求后，对上游发起独立的 WS 连接
//! - 两个 spawn task 分别处理 客户端→上游 和 上游→客户端 两个方向
//! - 任一方向断开即终止整个隧道
//!
//! 优势（vs TCP 隧道）：
//! - 可在应用层注入认证、日志、指标
//! - 与现有熔断器、限流器兼容

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite;

/// WebSocket 双向隧道：客户端 ↔ 上游。
///
/// # Arguments
/// * `client_ws` - Axum 升级后的客户端 WebSocket
/// * `upstream_url` - 上游 WS URL（ws:// 或 wss:// 协议）
/// * `_auth_token` - 可选认证令牌（预留，WS 握手不支持自定义 Header）
pub async fn ws_tunnel(
    mut client_ws: WebSocket,
    upstream_url: String,
    _auth_token: Option<String>,
) {
    tracing::info!(%upstream_url, "WebSocket 隧道建立中");

    // 1. 连接上游 WebSocket
    let upstream_ws = match tokio_tungstenite::connect_async(&upstream_url).await {
        Ok((ws, _)) => ws,
        Err(e) => {
            let reason = format!("上游 WebSocket 连接失败: {} (url={})", e, upstream_url);
            tracing::error!(%upstream_url, %e, "WebSocket 隧道建立失败 — 请检查: 1) fakesvc 是否在目标端口运行 2) 端点路径是否正确 3) strip_prefix 配置是否匹配");
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

    // 2. 客户端 → 上游
    let t1 = tokio::spawn(async move {
        while let Some(msg) = client_stream.next().await {
            let upstream_msg = match msg {
                Ok(Message::Text(t)) => {
                    tungstenite::Message::Text(t.to_string())
                }
                Ok(Message::Binary(b)) => tungstenite::Message::Binary(b.to_vec()),
                Ok(Message::Ping(d)) => tungstenite::Message::Ping(d.to_vec()),
                Ok(Message::Pong(d)) => tungstenite::Message::Pong(d.to_vec()),
                Ok(Message::Close(c)) => {
                    let frame = c.map(|f| tungstenite::protocol::CloseFrame {
                        code: tungstenite::protocol::frame::coding::CloseCode::from(f.code),
                        reason: f.reason,
                    });
                    let _ = upstream_sink
                        .send(tungstenite::Message::Close(frame))
                        .await;
                    break;
                }
                Err(e) => {
                    tracing::warn!("客户端 WS 读取错误: {}", e);
                    break;
                }
            };
            if upstream_sink.send(upstream_msg).await.is_err() {
                tracing::warn!("上游 WS sink 已关闭");
                break;
            }
        }
    });

    // 3. 上游 → 客户端
    let t2 = tokio::spawn(async move {
        while let Some(msg) = upstream_stream.next().await {
            let client_msg = match msg {
                Ok(tungstenite::Message::Text(t)) => Message::Text(t.into()),
                Ok(tungstenite::Message::Binary(b)) => Message::Binary(b),
                Ok(tungstenite::Message::Ping(d)) => Message::Ping(d),
                Ok(tungstenite::Message::Pong(d)) => Message::Pong(d),
                Ok(tungstenite::Message::Close(c)) => {
                    let frame = c.map(|f| CloseFrame {
                        code: f.code.into(),
                        reason: f.reason,
                    });
                    let _ = client_sink.send(Message::Close(frame)).await;
                    break;
                }
                Ok(tungstenite::Message::Frame(_)) => continue,
                Err(e) => {
                    tracing::warn!("上游 WS 流读取错误: {}", e);
                    break;
                }
            };
            if client_sink.send(client_msg).await.is_err() {
                tracing::warn!("客户端 WS sink 已关闭");
                break;
            }
        }
    });

    // 4. 任一方向断开即终止
    tokio::select! {
        _ = t1 => {
            tracing::info!("客户端→上游 方向断开");
        }
        _ = t2 => {
            tracing::info!("上游→客户端 方向断开");
        }
    }
    tracing::info!("WebSocket 隧道已关闭");
}
