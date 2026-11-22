//! 多上游池（issue #97）：轮询 + 连接失败自动切换。
//!
//! 验收：停掉池内一个实例，BFF 转发仍成功（不返回 502）。
mod common;

use bff::config::{InputMapping, OutputMapping, RouteDef, RouteType, RouteTypeConfig};
use common::{base_config, make_state, spawn_business, test_client};
use serde_json::json;
use std::net::SocketAddr;

/// 返回一个「刚被释放」的地址：连接必然被拒绝（模拟已停实例）。
async fn dead_addr() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    addr
}

fn pool_route(path: &str, upstreams: Vec<String>) -> RouteDef {
    RouteDef {
        sites: vec![],
        path: path.into(),
        methods: vec![],
        description: "多上游池".into(),
        auth_required: false,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstreams,
            strip_prefix: false,
            proxy_mode: "http".into(),
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    }
}

async fn live_upstream() -> String {
    let mock = wiremock::MockServer::start().await;
    // 不限制方法：GET 与 POST 用例共用同一上游
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&mock)
        .await;
    mock.uri()
}

#[tokio::test]
async fn failover_to_live_instance_when_one_is_down() {
    let dead = dead_addr().await;
    let live = live_upstream().await;
    let mut cfg = base_config();
    cfg.routes.push(pool_route(
        "/api/pool",
        vec![format!("http://{dead}"), live],
    ));
    let base = spawn_business(make_state(cfg)).await;
    let client = test_client();

    // 轮询起点在两个实例间轮转：必然有一半请求先打到已停实例并切换。
    for _ in 0..6 {
        let resp = client
            .get(format!("{base}/api/pool"))
            .send()
            .await
            .expect("请求应成功");
        assert_eq!(resp.status(), 200, "停一个实例后转发仍须成功");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
    }
}

#[tokio::test]
async fn failover_applies_to_non_idempotent_connect_failures() {
    // 连接未建立 = 请求未被上游接收，POST 切换不会重复写入。
    let dead = dead_addr().await;
    let live = live_upstream().await;
    let mut cfg = base_config();
    cfg.routes.push(pool_route(
        "/api/pool",
        vec![format!("http://{dead}"), live],
    ));
    let base = spawn_business(make_state(cfg)).await;
    let client = test_client();

    for _ in 0..4 {
        let resp = client
            .post(format!("{base}/api/pool"))
            .json(&json!({"k": "v"}))
            .send()
            .await
            .expect("请求应成功");
        assert_eq!(resp.status(), 200, "POST 在连接失败时也应切换实例");
    }
}

#[tokio::test]
async fn all_instances_down_returns_bad_gateway() {
    let mut cfg = base_config();
    cfg.routes.push(pool_route(
        "/api/pool",
        vec![
            format!("http://{}", dead_addr().await),
            format!("http://{}", dead_addr().await),
        ],
    ));
    let base = spawn_business(make_state(cfg)).await;
    let resp = test_client()
        .get(format!("{base}/api/pool"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 502);
}
