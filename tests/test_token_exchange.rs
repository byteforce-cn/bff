//! RFC 8693 Token Exchange 集成测试（TDD，对齐 docs/token-exchange-rfc8693.md §9）。
//!
//! 用 wiremock 模拟：
//! - 授权服务器（exchange token endpoint）：`POST /oauth2/token`
//! - 上游 resource server：校验注入的 Bearer token
//! - 会话刷新（T4/T6）复用 common 的 mock OIDC IdP

mod common;

use base64::Engine;
use bff::config::{
    InputMapping, OutputMapping, RouteDef, RouteType, RouteTypeConfig, TokenExchangeAuthMethod,
    TokenExchangeConfig,
};
use bff::oidc::StoredTokens;
use common::{base_config, make_state, spawn_admin, spawn_business, test_client};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tower_sessions::Session;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const AS_PATH: &str = "/oauth2/token";

fn exchange_route(upstream: &str, te: TokenExchangeConfig) -> RouteDef {
    RouteDef {
        path: "/ex".into(),
        methods: vec![],
        description: "token exchange 代理".into(),
        auth_required: true,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(upstream.to_string()),
            strip_prefix: false,
            proxy_mode: "http".into(),
            token_exchange: Some(te),
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    }
}

fn te_cfg(as_url: &str) -> TokenExchangeConfig {
    TokenExchangeConfig {
        token_endpoint: Some(format!("{}{}", as_url, AS_PATH)),
        client_id: "bff".into(),
        client_secret: "bff-secret".into(),
        client_auth_method: TokenExchangeAuthMethod::ClientSecretBasic,
        audience: vec!["admin-api".into()],
        scope: "admin.api".into(),
        cache_ttl: Duration::from_secs(60),
        ..Default::default()
    }
}

fn session_tokens(access: &str, refresh: Option<&str>) -> StoredTokens {
    StoredTokens::new("mock", "user-1", access, refresh, None, 3600).unwrap()
}

/// 创建带令牌的会话并返回 (Session, Cookie 头值)。
async fn make_session(state: &bff::state::AppState, tokens: &StoredTokens) -> (Session, String) {
    let session = Session::new(None, Arc::new(state.session_store.clone()), None);
    session
        .insert(&bff::oidc::tokens::session_key(&tokens.provider), tokens)
        .await
        .unwrap();
    session
        .insert("oidc:current_provider", &tokens.provider)
        .await
        .unwrap();
    session.save().await.unwrap();
    let id = session.id().unwrap().to_string();
    (session, format!("BFF_SESSION={}", id))
}

/// 解析 AS 收到的最后一个请求的 form 参数。
async fn as_form(as_server: &MockServer) -> HashMap<String, String> {
    let reqs = as_server.received_requests().await.unwrap();
    let last = reqs.last().expect("AS 应收到请求");
    let body = String::from_utf8(last.body.clone()).unwrap();
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

async fn as_count(as_server: &MockServer) -> usize {
    as_server
        .received_requests()
        .await
        .unwrap_or_default()
        .len()
}

// ============================================================
// T1: 交换后 token 注入上游
// ============================================================
#[tokio::test]
async fn t1_exchange_token_injected_to_upstream() {
    let as_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(AS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "exchanged-token-1",
            "token_type": "Bearer",
            "expires_in": 3600,
        })))
        .mount(&as_server)
        .await;

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ex/data"))
        .and(wiremock::matchers::header(
            "Authorization",
            "Bearer exchanged-token-1",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&upstream)
        .await;

    let mut cfg = base_config();
    cfg.routes
        .push(exchange_route(&upstream.uri(), te_cfg(&as_server.uri())));
    let state = make_state(cfg);
    let (_, cookie) = make_session(&state, &session_tokens("session-access-token", None)).await;
    let bff = spawn_business(state).await;
    let client = test_client();

    let resp = client
        .get(format!("{}/ex/data", bff))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "应代理成功");

    // AS 收到标准 RFC 8693 交换请求
    let form = as_form(&as_server).await;
    assert_eq!(
        form.get("grant_type").map(String::as_str),
        Some("urn:ietf:params:oauth:grant-type:token-exchange")
    );
    assert_eq!(
        form.get("subject_token").map(String::as_str),
        Some("session-access-token")
    );
    assert_eq!(
        form.get("subject_token_type").map(String::as_str),
        Some("urn:ietf:params:oauth:token-type:access_token")
    );
    assert_eq!(form.get("audience").map(String::as_str), Some("admin-api"));
    assert_eq!(form.get("scope").map(String::as_str), Some("admin.api"));
}

// ============================================================
// T2: 缓存复用（N 次请求仅 1 次交换）
// ============================================================
#[tokio::test]
async fn t2_cache_reuse_single_exchange() {
    let as_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(AS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "exchanged-token-1",
            "token_type": "Bearer",
            "expires_in": 3600,
        })))
        .expect(1)
        .mount(&as_server)
        .await;

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ex/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&upstream)
        .await;

    let mut cfg = base_config();
    cfg.routes
        .push(exchange_route(&upstream.uri(), te_cfg(&as_server.uri())));
    let state = make_state(cfg);
    let (_, cookie) = make_session(&state, &session_tokens("session-access-token", None)).await;
    let bff = spawn_business(state).await;
    let client = test_client();

    for _ in 0..3 {
        let resp = client
            .get(format!("{}/ex/data", bff))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }
    // TTL 内 3 次请求仅 1 次交换
    assert_eq!(as_count(&as_server).await, 1);
}

// ============================================================
// T3: TTL 生效（expires_in 短于 cache_ttl → 按 expires_in 过期重交换）
// ============================================================
#[tokio::test]
async fn t3_ttl_expires_re_exchange() {
    let as_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(AS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "exchanged-token-1",
            "token_type": "Bearer",
            "expires_in": 1,
        })))
        .mount(&as_server)
        .await;

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ex/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&upstream)
        .await;

    let mut cfg = base_config();
    cfg.routes
        .push(exchange_route(&upstream.uri(), te_cfg(&as_server.uri())));
    let state = make_state(cfg);
    let (_, cookie) = make_session(&state, &session_tokens("session-access-token", None)).await;
    let bff = spawn_business(state).await;
    let client = test_client();

    let req = || async {
        client
            .get(format!("{}/ex/data", bff))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
    };

    assert_eq!(req().await.status(), 200);
    assert_eq!(as_count(&as_server).await, 1);
    // expires_in=1 → 缓存 1s 后过期 → 再次交换
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(req().await.status(), 200);
    assert_eq!(as_count(&as_server).await, 2);
}

// ============================================================
// T4: 会话刷新后自动重交换（旧缓存不复用）
// ============================================================
#[tokio::test]
async fn t4_session_refresh_re_exchanges() {
    let idp = common::spawn_mock_oidc_provider().await;

    let as_server = MockServer::start().await;
    // 第一次交换 → exchanged-1；刷新后（subject 变化）→ exchanged-2
    Mock::given(method("POST"))
        .and(path(AS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "exchanged-1", "token_type": "Bearer", "expires_in": 3600,
        })))
        .up_to_n_times(1)
        .mount(&as_server)
        .await;
    Mock::given(method("POST"))
        .and(path(AS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "exchanged-2", "token_type": "Bearer", "expires_in": 3600,
        })))
        .mount(&as_server)
        .await;

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ex/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&upstream)
        .await;

    let mut cfg = base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    cfg.routes
        .push(exchange_route(&upstream.uri(), te_cfg(&as_server.uri())));
    let state = make_state(cfg);
    let tokens = session_tokens("session-access-token", Some("mock-refresh-token"));
    let (session, cookie) = make_session(&state, &tokens).await;
    let bff = spawn_business(state.clone()).await;
    let client = test_client();

    let req = || async {
        client
            .get(format!("{}/ex/data", bff))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
    };

    // 第一次请求 → 交换（exchanged-1）
    assert_eq!(req().await.status(), 200);
    assert_eq!(as_count(&as_server).await, 1);
    // 缓存命中，不再交换
    assert_eq!(req().await.status(), 200);
    assert_eq!(as_count(&as_server).await, 1);

    // 会话刷新 → subject token 变化 → 缓存键变化 → 再次交换（exchanged-2）
    bff::oidc::handlers::force_refresh(&state, &session, &tokens)
        .await
        .unwrap()
        .expect("刷新应成功");
    assert_eq!(req().await.status(), 200);
    assert_eq!(as_count(&as_server).await, 2, "刷新后应重新交换");

    // 验证上游第二次收到的是 exchanged-2（通过 AS 收到的 subject 变化佐证）
    let reqs = as_server.received_requests().await.unwrap();
    let second = &reqs[1];
    let body = String::from_utf8(second.body.clone()).unwrap();
    let params: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(
        params.get("subject_token").map(String::as_str),
        Some("mock-access-token-refreshed"),
        "刷新后应以新 access token 作为 subject_token"
    );
}

// ============================================================
// T5: 未配置 token_exchange 的路由行为不变（回归）
// ============================================================
#[tokio::test]
async fn t5_without_exchange_injects_session_token() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/plain/data"))
        .and(wiremock::matchers::header(
            "Authorization",
            "Bearer session-access-token",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&upstream)
        .await;

    let mut cfg = base_config();
    cfg.routes.push(RouteDef {
        path: "/plain".into(),
        methods: vec![],
        description: "普通代理".into(),
        auth_required: true,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(upstream.uri()),
            strip_prefix: false,
            proxy_mode: "http".into(),
            token_exchange: None,
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    });

    let state = make_state(cfg);
    let (_, cookie) = make_session(&state, &session_tokens("session-access-token", None)).await;
    let bff = spawn_business(state).await;
    let client = test_client();

    let resp = client
        .get(format!("{}/plain/data", bff))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "未配置 exchange 的路由应直接注入会话 token"
    );
}

// ============================================================
// T6: invalid_grant → 刷新会话 + 重试一次（成功后正常返回）
// ============================================================
#[tokio::test]
async fn t6_invalid_grant_refresh_and_retry() {
    let idp = common::spawn_mock_oidc_provider().await;

    let as_server = MockServer::start().await;
    // 第一次交换 → invalid_grant；刷新后重试 → 成功
    Mock::given(method("POST"))
        .and(path(AS_PATH))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "subject token expired",
        })))
        .up_to_n_times(1)
        .mount(&as_server)
        .await;
    Mock::given(method("POST"))
        .and(path(AS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "exchanged-after-refresh",
            "token_type": "Bearer",
            "expires_in": 3600,
        })))
        .mount(&as_server)
        .await;

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ex/data"))
        .and(wiremock::matchers::header(
            "Authorization",
            "Bearer exchanged-after-refresh",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&upstream)
        .await;

    let mut cfg = base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    cfg.routes
        .push(exchange_route(&upstream.uri(), te_cfg(&as_server.uri())));
    let state = make_state(cfg);
    let (_, cookie) = make_session(
        &state,
        &session_tokens("session-access-token", Some("mock-refresh-token")),
    )
    .await;
    let bff = spawn_business(state).await;
    let client = test_client();

    let resp = client
        .get(format!("{}/ex/data", bff))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "刷新并重试后应成功");
    assert_eq!(
        as_count(&as_server).await,
        2,
        "应交换两次（首次失败 + 重试）"
    );
    assert_eq!(
        idp.refresh_count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "应只刷新一次会话"
    );
}

// ============================================================
// T7: access_denied → 401，不重试、不刷新
// ============================================================
#[tokio::test]
async fn t7_access_denied_returns_401_no_retry() {
    let as_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(AS_PATH))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "access_denied",
            "error_description": "user lacks permission",
        })))
        .mount(&as_server)
        .await;

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ex/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("never"))
        .mount(&upstream)
        .await;

    let mut cfg = base_config();
    cfg.routes
        .push(exchange_route(&upstream.uri(), te_cfg(&as_server.uri())));
    let state = make_state(cfg);
    // 无 refresh token：即使误触发刷新也不会成功
    let (_, cookie) = make_session(&state, &session_tokens("session-access-token", None)).await;
    let bff = spawn_business(state).await;
    let client = test_client();

    let resp = client
        .get(format!("{}/ex/data", bff))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "access_denied 应映射为 401");
    assert_eq!(as_count(&as_server).await, 1, "不可重试：仅 1 次交换请求");
    assert_eq!(
        upstream.received_requests().await.unwrap().len(),
        0,
        "不应转发到上游"
    );
}

// ============================================================
// T8: token endpoint 5xx → 502，不触发会话刷新
// ============================================================
#[tokio::test]
async fn t8_token_endpoint_5xx_returns_502() {
    let as_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(AS_PATH))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&as_server)
        .await;

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ex/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("never"))
        .mount(&upstream)
        .await;

    let mut cfg = base_config();
    cfg.routes
        .push(exchange_route(&upstream.uri(), te_cfg(&as_server.uri())));
    let state = make_state(cfg);
    let (_, cookie) = make_session(&state, &session_tokens("session-access-token", None)).await;
    let bff = spawn_business(state).await;
    let client = test_client();

    let resp = client
        .get(format!("{}/ex/data", bff))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 502, "token endpoint 故障应映射为 502");
    assert_eq!(as_count(&as_server).await, 1);
    assert_eq!(
        upstream.received_requests().await.unwrap().len(),
        0,
        "不应转发到上游"
    );
}

// ============================================================
// T9: 客户端认证 basic / post
// ============================================================
#[tokio::test]
async fn t9a_client_secret_basic() {
    let as_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(AS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "exchanged-token-1",
            "token_type": "Bearer",
            "expires_in": 3600,
        })))
        .mount(&as_server)
        .await;

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ex/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&upstream)
        .await;

    let mut cfg = base_config();
    cfg.routes
        .push(exchange_route(&upstream.uri(), te_cfg(&as_server.uri())));
    let state = make_state(cfg);
    let (_, cookie) = make_session(&state, &session_tokens("session-access-token", None)).await;
    let bff = spawn_business(state).await;
    let client = test_client();

    let resp = client
        .get(format!("{}/ex/data", bff))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let reqs = as_server.received_requests().await.unwrap();
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("bff:bff-secret")
    );
    let auth = reqs[0]
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(auth, expected, "basic 认证应携带 Authorization 头");
    // basic 模式下 form 不应含 client_secret
    let form = as_form(&as_server).await;
    assert!(!form.contains_key("client_secret"));
    assert!(!form.contains_key("client_id"));
}

#[tokio::test]
async fn t9b_client_secret_post() {
    let as_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(AS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "exchanged-token-1",
            "token_type": "Bearer",
            "expires_in": 3600,
        })))
        .mount(&as_server)
        .await;

    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ex/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&upstream)
        .await;

    let mut cfg = base_config();
    let mut te = te_cfg(&as_server.uri());
    te.client_auth_method = TokenExchangeAuthMethod::ClientSecretPost;
    cfg.routes.push(exchange_route(&upstream.uri(), te));
    let state = make_state(cfg);
    let (_, cookie) = make_session(&state, &session_tokens("session-access-token", None)).await;
    let bff = spawn_business(state).await;
    let client = test_client();

    let resp = client
        .get(format!("{}/ex/data", bff))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let form = as_form(&as_server).await;
    assert_eq!(form.get("client_id").map(String::as_str), Some("bff"));
    assert_eq!(
        form.get("client_secret").map(String::as_str),
        Some("bff-secret")
    );
    let reqs = as_server.received_requests().await.unwrap();
    let auth = reqs[0]
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(auth.is_empty(), "post 模式不应携带 Authorization 头");
}

// ============================================================
// T10: 配置导入/导出回环（client_secret 打码、round-trip 不破坏）
// ============================================================
#[tokio::test]
async fn t10_config_export_import_roundtrip_masks_secret() {
    let as_server = MockServer::start().await;
    let mut cfg = base_config();
    let mut te = te_cfg(&as_server.uri());
    te.client_secret = "real-secret-value".into();
    cfg.routes.push(exchange_route("http://upstream:9000", te));
    let state = make_state(cfg);
    let admin = spawn_admin(state.clone()).await;
    let client = test_client();
    let auth = "test-admin-token";

    // 1. 导出：client_secret 已打码
    let resp = client
        .get(format!("{}/admin/api/config/export", admin))
        .header("x-admin-token", auth)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let yaml = resp.text().await.unwrap();
    assert!(yaml.contains("***"), "导出应打码: {}", yaml);
    assert!(!yaml.contains("real-secret-value"), "导出不应含真实密钥");

    // 2. 原样回导 → 成功且不破坏真实值（merge_sensitive_secrets 跳过覆盖）
    let resp = client
        .post(format!("{}/admin/api/config/import", admin))
        .header("x-admin-token", auth)
        .header("content-type", "application/yaml")
        .body(yaml.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "回导失败: {:?}", resp.text().await);

    let cur = state.cfg();
    let te = cur
        .routes
        .iter()
        .find(|r| r.path == "/ex")
        .and_then(|r| r.config.token_exchange.as_ref())
        .expect("路由应有 token_exchange");
    assert_eq!(
        te.client_secret, "real-secret-value",
        "导入哨兵后应保留当前已注入的密钥"
    );

    // 3. 再次导出仍打码
    let resp = client
        .get(format!("{}/admin/api/config/export", admin))
        .header("x-admin-token", auth)
        .send()
        .await
        .unwrap();
    let yaml2 = resp.text().await.unwrap();
    assert!(!yaml2.contains("real-secret-value"));
}
