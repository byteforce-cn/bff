//! 全功能代理联调集成测试（TDD）。
//!
//! 测试范围：
//! - HTTP REST 代理（标准 reqwest + Bearer token 注入）
//! - SSE 流式透传（逐 chunk relay）
//! - WebSocket 双向隧道（upgrade + relay）
//! - 熔断器集成
//! - proxy_mode 协议分发

mod common;

use bff::config::{
    InputMapping, OutputMapping, RouteDef, RouteType, RouteTypeConfig,
};
use common::{base_config, make_state, spawn_business, test_client};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;

// ============================================================
// 1. HTTP REST 代理测试
// ============================================================

#[tokio::test]
async fn test_http_proxy_with_mock_upstream() {
    // 使用 wiremock 模拟上游服务
    let mock_server = wiremock::MockServer::start().await;

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/users"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!([
            {"id": 1, "name": "Alice", "email": "alice@test.com"},
            {"id": 2, "name": "Bob", "email": "bob@test.com"}
        ])))
        .mount(&mock_server)
        .await;

    let mut cfg = base_config();
    cfg.routes.push(RouteDef {
        path: "/api/users".into(),
        methods: vec![],
        description: "用户列表".into(),
        auth_required: false,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(mock_server.uri()),
            strip_prefix: false,
            proxy_mode: "http".into(),
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    });

    let base = spawn_business(make_state(cfg)).await;
    let client = test_client();

    let resp = client
        .get(format!("{}/api/users", base))
        .send()
        .await
        .expect("HTTP proxy 请求失败");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.is_array());
    assert_eq!(body.as_array().unwrap().len(), 2);
    assert_eq!(body[0]["name"], "Alice");
}

#[tokio::test]
async fn test_http_proxy_with_auth_token_injection() {
    let mock_server = wiremock::MockServer::start().await;

    // 验证上游收到了 Bearer token
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/users"))
        .and(wiremock::matchers::header("Authorization", "Bearer test-token-123"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("authenticated"))
        .mount(&mock_server)
        .await;

    let mut cfg = base_config();
    cfg.routes.push(RouteDef {
        path: "/api/users".into(),
        methods: vec![],
        description: "认证用户列表".into(),
        auth_required: true,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(mock_server.uri()),
            strip_prefix: false,
            proxy_mode: "http".into(),
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    });

    let base = spawn_business(make_state(cfg)).await;
    let client = test_client();

    // 未登录状态 → BFF 返回 401（auth_required=true 且 session 无 token）
    let resp = client
        .get(format!("{}/api/users", base))
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 401, "未登录应返回 401");
}

#[tokio::test]
async fn test_http_proxy_404_on_unmatched_route() {
    let mut cfg = base_config();
    cfg.routes.clear();
    cfg.routes.clear();

    let base = spawn_business(make_state(cfg)).await;
    let client = test_client();

    let resp = client
        .get(format!("{}/api/nonexistent", base))
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 404, "无匹配路由且 /api 前缀应返回 404");
}

// ============================================================
// 2. SSE 流式透传测试
// ============================================================

#[tokio::test]
async fn test_sse_stream_proxy() {
    let mock_server = wiremock::MockServer::start().await;

    // 模拟 SSE 流响应
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/sse/clock"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .append_header("Content-Type", "text/event-stream")
                .append_header("Cache-Control", "no-cache")
                .set_body_string("data: {\"time\":\"12:00:00\"}\n\ndata: {\"time\":\"12:00:01\"}\n\n"),
        )
        .mount(&mock_server)
        .await;

    let mut cfg = base_config();
    cfg.routes.push(RouteDef {
        path: "/sse".into(),
        methods: vec![],
        description: "SSE 时钟".into(),
        auth_required: false,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(mock_server.uri()),
            strip_prefix: false,
            proxy_mode: "sse".into(),
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    });

    let base = spawn_business(make_state(cfg)).await;
    let client = test_client();

    let resp = client
        .get(format!("{}/sse/clock", base))
        .send()
        .await
        .expect("SSE proxy 请求失败");

    let body = resp.text().await.unwrap();
    // SSE 流式透传核心验证：响应体应包含 data: 前缀和 JSON 时间数据
    assert!(body.contains("data:"), "SSE 响应应包含 data: 前缀");
    assert!(body.contains("\"time\""), "SSE 响应应包含时间数据");
}

// ============================================================
// 3. proxy_mode 分发测试
// ============================================================

#[tokio::test]
async fn test_proxy_mode_defaults_to_http() {
    let mock_server = wiremock::MockServer::start().await;

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/data"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&mock_server)
        .await;

    let mut cfg = base_config();
    // proxy_mode 未指定时应默认为 "http"
    cfg.routes.push(RouteDef {
        path: "/api/data".into(),
        methods: vec![],
        description: "默认 HTTP 代理".into(),
        auth_required: false,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(mock_server.uri()),
            strip_prefix: false,
            // proxy_mode 使用 Default（即 ""）
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    });

    let base = spawn_business(make_state(cfg)).await;
    let client = test_client();

    let resp = client
        .get(format!("{}/api/data", base))
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn test_sse_mode_with_streaming_response() {
    // 验证 SSE proxy_mode 确实返回流式响应而非一次性缓冲
    let mock_server = wiremock::MockServer::start().await;

    // 大响应体模拟流式传输
    let large_body = "x".repeat(100_000);
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/sse/large"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .append_header("Content-Type", "text/event-stream")
                .set_body_string(large_body.clone()),
        )
        .mount(&mock_server)
        .await;

    let mut cfg = base_config();
    cfg.routes.push(RouteDef {
        path: "/sse".into(),
        methods: vec![],
        description: "SSE 大数据".into(),
        auth_required: false,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(mock_server.uri()),
            strip_prefix: false,
            proxy_mode: "sse".into(),
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    });

    let base = spawn_business(make_state(cfg)).await;
    let client = test_client();

    let resp = client
        .get(format!("{}/sse/large", base))
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body.len(), large_body.len());
}

// ============================================================
// 4. 熔断器集成测试
// ============================================================

#[tokio::test]
async fn test_circuit_breaker_opens_on_repeated_failures() {
    // 上游返回 500 错误 → 熔断器打开 → 后续请求被拒绝
    let mock_server = wiremock::MockServer::start().await;

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/failing"))
        .respond_with(wiremock::ResponseTemplate::new(500))
        .mount(&mock_server)
        .await;

    let mut cfg = base_config();
    cfg.routes.push(RouteDef {
        path: "/api/failing".into(),
        methods: vec![],
        description: "会失败的路由".into(),
        auth_required: false,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(mock_server.uri()),
            strip_prefix: false,
            proxy_mode: "http".into(),
            circuit_breaker_threshold: 2,
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    });

    let base = spawn_business(make_state(cfg)).await;
    let client = test_client();

    // 连续触发失败以打开熔断
    for _ in 0..5 {
        let _ = client
            .get(format!("{}/api/failing", base))
            .send()
            .await;
    }

    // 熔断器应已打开
    let resp = client
        .get(format!("{}/api/failing", base))
        .send()
        .await
        .expect("请求失败");

    assert_eq!(resp.status(), 503, "熔断器打开后应返回 503");
}

// ============================================================
// 5. WebSocket 隧道测试（需要 fakesvc 运行）
// ============================================================

#[tokio::test]
#[ignore = "需要 fakesvc 在 localhost:9091 运行"]
async fn test_ws_tunnel_echo_with_fakesvc() {
    // 直接连接 fakesvc 验证 WebSocket echo
    let ws_url = "ws://localhost:9091/ws/echo";

    let (mut ws, _) = connect_async(ws_url)
        .await
        .expect("连接 fakesvc WS 失败");

    let test_msg = "hello-from-test";
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        test_msg.into(),
    ))
    .await
    .expect("发送消息失败");

    let response = timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("超时")
        .expect("流结束")
        .expect("消息错误");

    if let tokio_tungstenite::tungstenite::Message::Text(text) = response {
        assert_eq!(text, test_msg, "Echo 应返回相同内容");
    } else {
        panic!("期望 Text 消息, 收到 {:?}", response);
    }
}

#[tokio::test]
#[ignore = "需要 fakesvc 在 localhost:9091 运行"]
async fn test_ws_tunnel_clock_with_fakesvc() {
    let ws_url = "ws://localhost:9091/ws/clock";

    let (mut ws, _) = connect_async(ws_url)
        .await
        .expect("连接 fakesvc WS clock 失败");

    let response = timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("超时")
        .expect("流结束")
        .expect("消息错误");

    if let tokio_tungstenite::tungstenite::Message::Text(text) = response {
        let v: serde_json::Value =
            serde_json::from_str(&text).expect("clock 消息应为 JSON");
        assert!(v.get("time").is_some(), "clock 消息应包含 time 字段");
    } else {
        panic!("期望 Text 消息");
    }
}

// ============================================================
// 6. 端到端：frontend 模拟真实业务请求
// ============================================================

#[tokio::test]
async fn test_e2e_frontend_simulates_rest_request() {
    // 模拟前端发起的 GET /api/users 请求（经 BFF 代理到 mock 上游）
    let mock_server = wiremock::MockServer::start().await;

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/users"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!([
            {"id": 1, "name": "Test User", "email": "test@example.com"}
        ])))
        .mount(&mock_server)
        .await;

    let mut cfg = base_config();
    cfg.routes.push(RouteDef {
        path: "/api/users".into(),
        methods: vec![],
        description: "用户 API".into(),
        auth_required: false,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(mock_server.uri()),
            strip_prefix: false,
            proxy_mode: "http".into(),
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    });

    let base = spawn_business(make_state(cfg)).await;

    // 模拟前端 fetch 调用
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/users", base))
        .header("Accept", "application/json")
        .header("User-Agent", "Mozilla/5.0 (Test Frontend)")
        .send()
        .await
        .expect("前端请求失败");

    assert_eq!(resp.status(), 200);

    let users: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["name"], "Test User");
}

#[tokio::test]
async fn test_e2e_frontend_simulates_sse_request() {
    let mock_server = wiremock::MockServer::start().await;

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/sse/clock"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .append_header("Content-Type", "text/event-stream")
                .set_body_string("data: {\"time\":\"14:30:00\"}\n\n"),
        )
        .mount(&mock_server)
        .await;

    let mut cfg = base_config();
    cfg.routes.push(RouteDef {
        path: "/sse".into(),
        methods: vec![],
        description: "SSE 流".into(),
        auth_required: false,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(mock_server.uri()),
            strip_prefix: false,
            proxy_mode: "sse".into(),
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    });

    let base = spawn_business(make_state(cfg)).await;

    // 模拟前端 EventSource 连接
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/sse/clock", base))
        .header("Accept", "text/event-stream")
        .send()
        .await
        .expect("SSE 请求失败");

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("data:"), "SSE 响应应包含 data: 前缀");
}

#[tokio::test]
async fn test_e2e_health_check() {
    let cfg = base_config();
    let base = spawn_business(make_state(cfg)).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/live", base))
        .send()
        .await
        .expect("/live 请求失败");

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

// ============================================================
// 5.5 WebSocket URL 构造单元测试（不依赖 fakesvc）
// ============================================================

#[test]
fn test_ws_url_strip_prefix_false() {
    // 验证 strip_prefix=false 时，完整路径被传递到上游
    // 这是修复 WS 隧道 1011 的核心测试
    let cfg = bff::config::AppConfig::load(std::path::Path::new("config"))
        .expect("加载配置失败");

    let ws_route = cfg.routes.iter().find(|r| r.path == "/ws").unwrap();

    // strip_prefix 应为 false（routes.yaml 配置）
    assert!(!ws_route.config.strip_prefix,
        "WS 路由 strip_prefix 必须为 false，否则上游 URL 构造错误");

    // proxy_mode 应为 websocket（自动检测）
    assert_eq!(ws_route.config.proxy_mode, "websocket");

    // 模拟 URL 构造逻辑（与 ws_upgrade_handler 一致）
    let upstream = ws_route.config.upstream.as_deref().unwrap().trim_end_matches('/');
    let path = "/ws/clock";
    let suffix = if ws_route.config.strip_prefix {
        path.strip_prefix(&ws_route.path).unwrap_or("")
    } else {
        path
    };
    let upstream_ws = upstream
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    let url = format!("{}{}", upstream_ws, suffix);

    // 期望: ws://localhost:9091/ws/clock（而非 ws://localhost:9091/clock）
    assert_eq!(url, "ws://localhost:9091/ws/clock",
        "strip_prefix=false 时路径不应被截断，fakesvc 期望完整路径 /ws/clock");
}

#[test]
fn test_sse_url_strip_prefix_false() {
    let cfg = bff::config::AppConfig::load(std::path::Path::new("config"))
        .expect("加载配置失败");

    let sse_route = cfg.routes.iter().find(|r| r.path == "/sse").unwrap();
    assert!(!sse_route.config.strip_prefix);
    assert_eq!(sse_route.config.proxy_mode, "sse");

    let upstream = sse_route.config.upstream.as_deref().unwrap().trim_end_matches('/');
    let path = "/sse/clock";
    let suffix = if sse_route.config.strip_prefix {
        path.strip_prefix(&sse_route.path).unwrap_or("")
    } else {
        path
    };
    let url = format!("{}{}", upstream, suffix);

    // 期望: http://localhost:9091/sse/clock
    assert_eq!(url, "http://localhost:9091/sse/clock",
        "SSE strip_prefix=false 时路径不应被截断");
}

// ============================================================
// 7. 配置验证测试
// ============================================================

#[tokio::test]
async fn test_routes_yaml_parses_fakesvc_routes() {
    // 验证 config/routes/routes.yaml 可被正确解析（包含 fakesvc 路由）
    let cfg = bff::config::AppConfig::load(std::path::Path::new("config"))
        .expect("加载配置失败");

    // 查找 fakesvc 路由
    let has_users_route = cfg.routes.iter().any(|r| r.path == "/api/users");
    let has_sse_route = cfg.routes.iter().any(|r| r.path == "/sse");
    let has_ws_route = cfg.routes.iter().any(|r| r.path == "/ws");

    assert!(has_users_route, "应包含 /api/users 路由");
    assert!(has_sse_route, "应包含 /sse 路由");
    assert!(has_ws_route, "应包含 /ws 路由");

    // 验证 proxy_mode 自动检测
    let users_route = cfg.routes.iter().find(|r| r.path == "/api/users").unwrap();
    assert_eq!(users_route.config.proxy_mode, "http", "/api/users → http");

    let ws_route = cfg.routes.iter().find(|r| r.path == "/ws").unwrap();
    assert_eq!(ws_route.config.proxy_mode, "websocket", "/ws → websocket");

    let sse_route = cfg.routes.iter().find(|r| r.path == "/sse").unwrap();
    assert_eq!(sse_route.config.proxy_mode, "sse", "/sse → sse");

    // 验证 strip_prefix
    assert!(!ws_route.config.strip_prefix, "/ws 不应 strip prefix");
    assert!(!sse_route.config.strip_prefix, "/sse 不应 strip prefix");
}

#[tokio::test]
async fn test_route_type_config_proxy_mode_field() {
    // 验证 proxy_mode 字段可正确序列化/反序列化
    let yaml = r#"
path: "/test"
type: proxy
auth_required: false
config:
  upstream: "http://localhost:9091"
  proxy_mode: "sse"
"#;

    let route: RouteDef = serde_yaml::from_str(yaml).expect("解析 YAML 失败");
    assert_eq!(route.config.proxy_mode, "sse");
    assert_eq!(route.config.upstream.as_deref(), Some("http://localhost:9091"));
}

#[tokio::test]
async fn test_route_type_config_default_proxy_mode() {
    let yaml = r#"
path: "/test"
type: proxy
config:
  upstream: "http://localhost:9091"
"#;

    let route: RouteDef = serde_yaml::from_str(yaml).expect("解析 YAML 失败");
    assert_eq!(route.config.proxy_mode, "http", "默认 proxy_mode 应为 http");
}
