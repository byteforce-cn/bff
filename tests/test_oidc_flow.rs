//! 场景 1：OIDC 完整登录流程 — 授权码回调 → 令牌存储 → Session 创建 → 认证代理。
mod common;

use bff::config::{InputMapping, OutputMapping, RouteDef, RouteType, RouteTypeConfig};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn oidc_full_login_flow() {
    let idp = common::spawn_mock_oidc_provider().await;

    // 下游服务：必须收到 Bearer mock-access-token
    let downstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .and(header("authorization", "Bearer mock-access-token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"name": "Alice"})),
        )
        .expect(1)
        .mount(&downstream)
        .await;

    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    cfg.routes.push(RouteDef {
        path: "/api/users".into(),
        methods: vec![],
        description: String::new(),
        auth_required: true,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(downstream.uri()),
            strip_prefix: true,
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    });
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    // 1. /login → 302 到 mock IdP
    let resp = client.get(format!("{}/login", bff)).send().await.unwrap();
    assert!(resp.status().is_redirection(), "应为 3xx 重定向");
    let location = resp.headers()["location"].to_str().unwrap().to_string();
    assert!(
        location.starts_with(&format!("{}/authorize", idp.url)),
        "应重定向到 IdP: {}",
        location
    );
    let auth_url = url::Url::parse(&location).unwrap();
    let params: std::collections::HashMap<_, _> = auth_url.query_pairs().into_owned().collect();
    let state_param = params.get("state").expect("授权 URL 应含 state").clone();
    let nonce = params.get("nonce").expect("授权 URL 应含 nonce").clone();
    assert!(params.contains_key("code_challenge"), "应使用 PKCE");
    *idp.nonce.lock().unwrap() = Some(nonce);

    // 2. 模拟 IdP 回调
    let resp = client
        .get(format!(
            "{}/auth/callback?code=mock-code&state={}",
            bff, state_param
        ))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_redirection(), "回调应重定向: {:?}", resp);

    // 3. 带会话访问受保护代理资源
    let resp = client
        .get(format!("{}/api/users/users/1", bff))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "Alice");

    downstream.verify().await;
}

#[tokio::test]
async fn callback_rejects_bad_state() {
    let idp = common::spawn_mock_oidc_provider().await;
    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    client.get(format!("{}/login", bff)).send().await.unwrap();
    let resp = client
        .get(format!("{}/auth/callback?code=x&state=forged", bff))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}
