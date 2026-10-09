//! WebSocket 隧道（`src/server/tunnel.rs`）集成测试。
//!
//! 背景：WS 隧道此前无自动化用例（`tunnel.rs` 覆盖率 0%）。
//! 本文件用**进程内 WS 回显上游**补齐主链路与全部加固分支：
//!
//! - 双向 relay（文本/二进制）与干净关闭（Close 透传 + 上游关闭帧记录）；
//! - 鉴权：`auth_required` 无会话 → 401（且不触及上游）；有会话 → 上游握手收到 `Bearer`；
//! - 路由约束：非 `websocket|auto` 模式 → 400；无匹配路由 → 404；
//! - 上游连接被拒 / 握手超时（黑洞上游）→ 客户端收到 1011 Close；
//! - 大小上限：客户端→上游（上游收到 1009）/ 上游→客户端（客户端收到 1009）；
//! - 心跳保活（空闲窗口内隧道保持）与空闲超时关闭（无心跳时）。

mod common;

use axum::extract::ws::{Message as AxMessage, WebSocketUpgrade};
use axum::extract::State as AxState;
use axum::http::HeaderMap;
use axum::response::Response as AxResponse;
use axum::routing::get;
use axum::Router;
use bff::config::{
    InputMapping, OutputMapping, RouteDef, RouteType, RouteTypeConfig, WebSocketTunnelConfig,
};
use common::{base_config, login_cookie, make_state, spawn_business};
use futures::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{header, HeaderValue};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

// ============================================================
// 进程内 WS 回显上游
// ============================================================

#[derive(Clone)]
struct UpstreamState {
    /// 升级握手时收到的 Authorization 头（None = 未收到）
    auth_header: Arc<Mutex<Option<String>>>,
    /// 收到的 Close 帧（外层 Some = 收到过；内层 = 关闭码）
    close_code: Arc<Mutex<Option<Option<u16>>>>,
    /// 非空时：升级后立即向 BFF 发送该大小的文本消息
    oversize_on_connect: Arc<Mutex<Option<usize>>>,
}

impl Default for UpstreamState {
    fn default() -> Self {
        Self {
            auth_header: Arc::new(Mutex::new(None)),
            close_code: Arc::new(Mutex::new(None)),
            oversize_on_connect: Arc::new(Mutex::new(None)),
        }
    }
}

async fn spawn_ws_upstream() -> (String, UpstreamState) {
    spawn_ws_upstream_with(UpstreamState::default()).await
}

async fn spawn_ws_upstream_with(st: UpstreamState) -> (String, UpstreamState) {
    let app = Router::new()
        .route("/ws", get(upstream_ws))
        .with_state(st.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://127.0.0.1:{}", addr.port()), st)
}

async fn upstream_ws(
    AxState(st): AxState<UpstreamState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> AxResponse {
    if let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        *st.auth_header.lock().unwrap() = Some(v.to_string());
    }
    let oversize = *st.oversize_on_connect.lock().unwrap();
    ws.on_upgrade(move |socket| upstream_session(st, socket, oversize))
}

async fn upstream_session(
    st: UpstreamState,
    socket: axum::extract::ws::WebSocket,
    oversize: Option<usize>,
) {
    let (mut sink, mut stream) = socket.split();
    if let Some(n) = oversize {
        let _ = sink.send(AxMessage::Text("x".repeat(n))).await;
    }
    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            AxMessage::Text(t) => {
                if sink
                    .send(AxMessage::Text(format!("echo:{t}")))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            AxMessage::Binary(b) => {
                if sink.send(AxMessage::Binary(b)).await.is_err() {
                    break;
                }
            }
            AxMessage::Ping(d) => {
                let _ = sink.send(AxMessage::Pong(d)).await;
            }
            AxMessage::Pong(_) => {}
            AxMessage::Close(c) => {
                *st.close_code.lock().unwrap() = Some(c.map(|f| f.code));
                let _ = sink.send(AxMessage::Close(None)).await;
                break;
            }
        }
    }
}

/// 黑洞上游：接受 TCP 连接但永不响应 WS 握手（用于握手超时用例）。
async fn spawn_blackhole() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        // 持有连接，不发送任何字节
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock);
        }
    });
    addr
}

// ============================================================
// 测试工具
// ============================================================

fn ws_route(upstream: &str, auth_required: bool, proxy_mode: &str) -> RouteDef {
    RouteDef {
        sites: vec![],
        path: "/ws".into(),
        methods: vec![],
        description: "WS 隧道".into(),
        auth_required,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(upstream.to_string()),
            strip_prefix: false,
            proxy_mode: proxy_mode.into(),
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    }
}

/// http://127.0.0.1:port → ws://127.0.0.1:port，并按需携带会话 Cookie。
///
/// clippy：`Err` 变体（含 http Response）较大 → 显式 allow（测试脚手架）。
#[allow(clippy::result_large_err)]
async fn ws_connect(
    base: &str,
    path: &str,
    cookie: Option<&str>,
) -> Result<
    (
        Ws,
        tokio_tungstenite::tungstenite::handshake::client::Response,
    ),
    tokio_tungstenite::tungstenite::Error,
> {
    let url = format!("ws{}", &base["http".len()..]) + path;
    let mut req = url.into_client_request().unwrap();
    if let Some(c) = cookie {
        req.headers_mut()
            .insert(header::COOKIE, HeaderValue::from_str(c).unwrap());
    }
    connect_async(req).await
}

fn err_status(e: &tokio_tungstenite::tungstenite::Error) -> Option<u16> {
    match e {
        tokio_tungstenite::tungstenite::Error::Http(resp) => Some(resp.status().as_u16()),
        _ => None,
    }
}

/// 读到下一条文本消息（跳过 ping/pong）。
async fn next_text(ws: &mut Ws, t: Duration) -> String {
    let deadline = tokio::time::Instant::now() + t;
    loop {
        let rem = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!rem.is_zero(), "等待文本消息超时");
        match tokio::time::timeout(rem, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => return t.to_string(),
            Ok(Some(Ok(_))) => continue,
            other => panic!("等待文本消息失败: {other:?}"),
        }
    }
}

/// 等待 Close 帧；返回 `Some(关闭码)`（无码关闭为 `Some(None)`）；连接先断开则 `None`。
async fn read_until_close(ws: &mut Ws, t: Duration) -> Option<Option<u16>> {
    let deadline = tokio::time::Instant::now() + t;
    loop {
        let rem = deadline.saturating_duration_since(tokio::time::Instant::now());
        if rem.is_zero() {
            return None;
        }
        match tokio::time::timeout(rem, ws.next()).await {
            Ok(Some(Ok(Message::Close(c)))) => return Some(c.map(|f| u16::from(f.code))),
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) => return None,
            Err(_) => return None,
        }
    }
}

/// 等待连接终止（Close 帧、协议错误或 EOF 均视为终止）。
async fn wait_terminal(ws: &mut Ws, t: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + t;
    loop {
        let rem = deadline.saturating_duration_since(tokio::time::Instant::now());
        if rem.is_zero() {
            return false;
        }
        match tokio::time::timeout(rem, ws.next()).await {
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) => return true,
            Err(_) => return false,
        }
    }
}

/// 轮询等待条件成立（跨任务状态断言用）。
async fn wait_for(mut f: impl FnMut() -> bool, t: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + t;
    while tokio::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f()
}

// ============================================================
// 1. 双向 relay 与干净关闭
// ============================================================

#[tokio::test]
async fn ws_echo_relay_and_clean_close() {
    let (upstream, st) = spawn_ws_upstream().await;
    let mut cfg = base_config();
    cfg.routes.push(ws_route(&upstream, false, "websocket"));
    let base = spawn_business(make_state(cfg)).await;

    let (mut ws, _) = ws_connect(&base, "/ws", None).await.expect("WS 升级应成功");

    // 文本双向 relay
    ws.send(Message::Text("hello".into())).await.unwrap();
    assert_eq!(
        next_text(&mut ws, Duration::from_secs(3)).await,
        "echo:hello"
    );

    // 二进制 relay（逐字节一致）
    ws.send(Message::Binary(vec![1, 2, 3, 250])).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let rem = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!rem.is_zero(), "等待二进制回显超时");
        match tokio::time::timeout(rem, ws.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => {
                assert_eq!(b.as_slice(), &[1, 2, 3, 250]);
                break;
            }
            Ok(Some(Ok(_))) => continue,
            other => panic!("等待二进制回显失败: {other:?}"),
        }
    }

    // 客户端关闭 → BFF 透传 Close 到上游 → 上游记录关闭帧（无码）
    ws.close(None).await.unwrap();
    let recorded = wait_for(
        || st.close_code.lock().unwrap().is_some(),
        Duration::from_secs(3),
    )
    .await;
    assert!(recorded, "上游应收到 Close 帧（客户端正常关闭被透传）");
    assert_eq!(*st.close_code.lock().unwrap(), Some(None));

    // 匿名路由不应向上游注入 Authorization
    assert!(st.auth_header.lock().unwrap().is_none());
}

// ============================================================
// 2. 鉴权与 Bearer 注入
// ============================================================

#[tokio::test]
async fn ws_auth_required_rejects_anonymous_and_injects_bearer() {
    let (upstream, st) = spawn_ws_upstream().await;
    let mut cfg = base_config();
    cfg.routes.push(ws_route(&upstream, true, "websocket"));
    let state = make_state(cfg);
    let cookie = login_cookie(&state).await;
    let base = spawn_business(state).await;

    // 匿名：401，且不触及上游（不建立上游连接）
    let err = ws_connect(&base, "/ws", None)
        .await
        .expect_err("匿名 WS 升级应被拒绝");
    assert_eq!(err_status(&err), Some(401), "应为 401: {err:?}");
    assert!(
        st.auth_header.lock().unwrap().is_none(),
        "鉴权失败不应触及上游"
    );
    assert!(st.close_code.lock().unwrap().is_none());

    // 已登录：升级成功，上游握手收到会话访问令牌
    let (mut ws, _) = ws_connect(&base, "/ws", Some(&cookie))
        .await
        .expect("已登录 WS 升级应成功");
    ws.send(Message::Text("hi".into())).await.unwrap();
    assert_eq!(next_text(&mut ws, Duration::from_secs(3)).await, "echo:hi");
    assert_eq!(
        st.auth_header.lock().unwrap().as_deref(),
        Some("Bearer test-access-token"),
        "auth_required 路由必须向上游注入 Bearer"
    );
}

// ============================================================
// 3. 路由约束
// ============================================================

#[tokio::test]
async fn ws_route_constraints() {
    // 非 websocket/auto 模式 → 400
    let (upstream, _st) = spawn_ws_upstream().await;
    let mut cfg = base_config();
    cfg.routes.push(ws_route(&upstream, false, "http"));
    let base = spawn_business(make_state(cfg)).await;
    let err = ws_connect(&base, "/ws", None)
        .await
        .expect_err("http 模式路由不应允许 WS 升级");
    assert_eq!(err_status(&err), Some(400), "应为 400: {err:?}");

    // 无匹配路由 → 404
    let cfg = base_config();
    let base = spawn_business(make_state(cfg)).await;
    let err = ws_connect(&base, "/ws", None)
        .await
        .expect_err("无匹配路由不应允许 WS 升级");
    assert_eq!(err_status(&err), Some(404), "应为 404: {err:?}");
}

// ============================================================
// 4. 上游连接失败 / 握手超时
// ============================================================

#[tokio::test]
async fn ws_upstream_connect_failure_closes_1011() {
    // 绑定后立即释放 → 该端口连接必然被拒
    let dead_port = {
        let l = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        l.local_addr().unwrap().port()
    };
    let mut cfg = base_config();
    cfg.routes.push(ws_route(
        &format!("http://127.0.0.1:{dead_port}"),
        false,
        "websocket",
    ));
    let base = spawn_business(make_state(cfg)).await;

    let (mut ws, _) = ws_connect(&base, "/ws", None)
        .await
        .expect("对客户端的升级应成功（失败发生在上游侧）");
    assert_eq!(
        read_until_close(&mut ws, Duration::from_secs(3)).await,
        Some(Some(1011)),
        "上游连接失败应向客户端回送 1011"
    );
}

#[tokio::test]
async fn ws_upstream_handshake_timeout_closes_1011() {
    let blackhole = spawn_blackhole().await;
    let mut cfg = base_config();
    cfg.websocket = WebSocketTunnelConfig {
        connect_timeout: Duration::from_millis(500),
        ..Default::default()
    };
    cfg.routes
        .push(ws_route(&format!("http://{blackhole}"), false, "websocket"));
    let base = spawn_business(make_state(cfg)).await;

    let started = tokio::time::Instant::now();
    let (mut ws, _) = ws_connect(&base, "/ws", None).await.unwrap();
    assert_eq!(
        read_until_close(&mut ws, Duration::from_secs(5)).await,
        Some(Some(1011)),
        "上游握手超时应向客户端回送 1011"
    );
    // 配置的 500ms 生效（而非默认 5s）：控制流应在 ~1s 内返回
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "握手超时应按配置（500ms）触发，实际 {:?}",
        started.elapsed()
    );
}

// ============================================================
// 5. 消息大小上限
// ============================================================

#[tokio::test]
async fn ws_oversized_client_message_closed_with_1009() {
    let (upstream, st) = spawn_ws_upstream().await;
    let mut cfg = base_config();
    cfg.websocket = WebSocketTunnelConfig {
        max_message_bytes: 64,
        heartbeat_interval: Duration::ZERO,
        idle_timeout: Duration::ZERO,
        ..Default::default()
    };
    cfg.routes.push(ws_route(&upstream, false, "websocket"));
    let base = spawn_business(make_state(cfg)).await;

    let (mut ws, _) = ws_connect(&base, "/ws", None).await.unwrap();
    ws.send(Message::Text("y".repeat(512))).await.unwrap();

    // 上游应收到 1009（Message Too Big）关闭帧
    let got = wait_for(
        || *st.close_code.lock().unwrap() == Some(Some(1009)),
        Duration::from_secs(3),
    )
    .await;
    assert!(
        got,
        "上游应收到 1009 关闭帧: {:?}",
        st.close_code.lock().unwrap()
    );
    // 客户端连接应终止（不悬挂）
    assert!(
        wait_terminal(&mut ws, Duration::from_secs(3)).await,
        "客户端连接应在超限后终止"
    );
}

#[tokio::test]
async fn ws_oversized_upstream_message_closes_client_1009() {
    let st = UpstreamState::default();
    *st.oversize_on_connect.lock().unwrap() = Some(4096);
    let (upstream, _st) = spawn_ws_upstream_with(st).await;

    let mut cfg = base_config();
    cfg.websocket = WebSocketTunnelConfig {
        max_message_bytes: 1024,
        heartbeat_interval: Duration::ZERO,
        idle_timeout: Duration::ZERO,
        ..Default::default()
    };
    cfg.routes.push(ws_route(&upstream, false, "websocket"));
    let base = spawn_business(make_state(cfg)).await;

    let (mut ws, _) = ws_connect(&base, "/ws", None).await.unwrap();
    assert_eq!(
        read_until_close(&mut ws, Duration::from_secs(3)).await,
        Some(Some(1009)),
        "上游超限消息应以 1009 关闭客户端"
    );
}

// ============================================================
// 6. 心跳保活与空闲超时
// ============================================================

#[tokio::test]
async fn ws_heartbeat_keeps_tunnel_alive_past_idle_timeout() {
    let (upstream, _st) = spawn_ws_upstream().await;
    let mut cfg = base_config();
    cfg.websocket = WebSocketTunnelConfig {
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_millis(500),
        heartbeat_interval: Duration::from_millis(100),
        max_message_bytes: 1024 * 1024,
    };
    cfg.routes.push(ws_route(&upstream, false, "websocket"));
    let base = spawn_business(make_state(cfg)).await;

    let (mut ws, _) = ws_connect(&base, "/ws", None).await.unwrap();

    // 保持 1s 无业务消息（> 2× 空闲超时）：心跳（Ping/Pong）应阻止隧道关闭。
    // 若心跳失效（回归），空闲超时会在 ~500ms 关闭隧道，下面的读循环将 panic。
    let until = tokio::time::Instant::now() + Duration::from_millis(1000);
    while tokio::time::Instant::now() < until {
        let rem = until.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(rem, ws.next()).await {
            Ok(Some(Ok(Message::Ping(d)))) => {
                ws.send(Message::Pong(d)).await.unwrap();
            }
            Ok(Some(Ok(Message::Pong(_)))) => {}
            Ok(Some(Ok(other))) => panic!("空闲窗口内不应收到其它消息: {other:?}"),
            Ok(Some(Err(e))) => panic!("隧道在空闲窗口内异常关闭: {e}"),
            Ok(None) => panic!("隧道在空闲窗口内被对端关闭"),
            Err(_) => {} // 该片段无消息：正常
        }
    }

    // 空闲窗口后仍可双向收发
    ws.send(Message::Text("alive".into())).await.unwrap();
    assert_eq!(
        next_text(&mut ws, Duration::from_secs(3)).await,
        "echo:alive"
    );
}

#[tokio::test]
async fn ws_idle_timeout_closes_tunnel_when_no_heartbeat() {
    let (upstream, _st) = spawn_ws_upstream().await;
    let mut cfg = base_config();
    cfg.websocket = WebSocketTunnelConfig {
        connect_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_millis(300),
        heartbeat_interval: Duration::ZERO, // 禁用心跳 → 空闲必须触发关闭
        max_message_bytes: 1024 * 1024,
    };
    cfg.routes.push(ws_route(&upstream, false, "websocket"));
    let base = spawn_business(make_state(cfg)).await;

    let (mut ws, _) = ws_connect(&base, "/ws", None).await.unwrap();
    assert!(
        wait_terminal(&mut ws, Duration::from_secs(2)).await,
        "无心跳时空闲超时应关闭隧道"
    );
}
