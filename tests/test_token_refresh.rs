//! 场景 7：令牌刷新 — Stale-While-Revalidate + 防惊群 + 代理层 401 重试。
mod common;

use bff::config::{RouteDef, RouteType, RouteTypeConfig, InputMapping, OutputMapping};
use bff::oidc::StoredTokens;
use std::sync::atomic::Ordering;
use std::time::Duration;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ============================================================
// 1. 中间件：token 未临期 → 不触发刷新
// ============================================================

#[tokio::test]
async fn test_valid_token_passes_through_no_refresh() {
    let idp = common::spawn_mock_oidc_provider().await;
    let downstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&downstream)
        .await;

    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    cfg.routes.push(RouteDef {
        path: "/api/data".into(),
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

    // Token 还有 3600s 才过期，远超 skew(60s)，不应触发刷新
    let tokens = StoredTokens::new(
        "mock", "user-1",
        "fresh-token", Some("mock-refresh-token"), None,
        3600,
    ).unwrap();
    let cookie = common::create_session_with_tokens(&state, &tokens).await;

    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    let resp = client
        .get(format!("{}/api/data/data", bff))
        .header("cookie", &cookie)
        .send().await.unwrap();

    assert_eq!(resp.status(), 200, "未临期 token 应直接放行");
    assert_eq!(idp.refresh_count.load(Ordering::SeqCst), 0, "不应触发刷新");
}

// ============================================================
// 2. 中间件：token 临期 → Stale-While-Revalidate（旧 token 放行，后台异步刷新）
// ============================================================

#[tokio::test]
async fn test_stale_while_revalidate_background_refresh() {
    let idp = common::spawn_mock_oidc_provider().await;
    let downstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&downstream)
        .await;

    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    cfg.routes.push(RouteDef {
        path: "/api/data".into(),
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

    // Token 30s 后过期，skew=60s → is_expiring(60) = true
    let tokens = StoredTokens::new(
        "mock", "user-1",
        "old-access-token", Some("mock-refresh-token"), None,
        30,
    ).unwrap();
    let cookie = common::create_session_with_tokens(&state, &tokens).await;

    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    // 请求应立即成功（使用旧 token，不阻塞）
    let resp = client
        .get(format!("{}/api/data/data", bff))
        .header("cookie", &cookie)
        .send().await.unwrap();
    assert_eq!(resp.status(), 200, "临期 token 应直接放行（Stale-While-Revalidate）");

    // 等待后台刷新完成
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(idp.refresh_count.load(Ordering::SeqCst), 1, "后台应触发一次刷新");

    // 后续请求应使用刷新后的新 token
    let resp = client
        .get(format!("{}/api/data/data", bff))
        .header("cookie", &cookie)
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let requests = downstream.received_requests().await.unwrap();
    let last = requests.last().unwrap();
    assert_eq!(
        last.headers.get("authorization").unwrap(),
        "Bearer mock-access-token-refreshed",
        "刷新后应使用新令牌"
    );
}

// ============================================================
// 3. 中间件：token 已过期 → 阻塞等待刷新
// ============================================================

#[tokio::test]
async fn test_expired_token_blocks_until_refreshed() {
    let idp = common::spawn_mock_oidc_provider().await;
    let downstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&downstream)
        .await;

    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    cfg.routes.push(RouteDef {
        path: "/api/data".into(),
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

    // Token 已过期 120s
    let tokens = StoredTokens::new(
        "mock", "user-1",
        "expired-token", Some("mock-refresh-token"), None,
        -120,
    ).unwrap();
    let cookie = common::create_session_with_tokens(&state, &tokens).await;

    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    let resp = client
        .get(format!("{}/api/data/data", bff))
        .header("cookie", &cookie)
        .send().await.unwrap();

    assert_eq!(resp.status(), 200, "过期 token 刷新后应成功");
    assert_eq!(idp.refresh_count.load(Ordering::SeqCst), 1, "应触发同步刷新");

    // 确认使用了新 token
    let requests = downstream.received_requests().await.unwrap();
    let last = requests.last().unwrap();
    assert_eq!(
        last.headers.get("authorization").unwrap(),
        "Bearer mock-access-token-refreshed",
        "应使用刷新后的新令牌"
    );
}

// ============================================================
// 4. 中间件：并发请求仅刷新一次（保留防惊群）
// ============================================================

#[tokio::test]
async fn concurrent_requests_trigger_single_refresh() {
    let idp = common::spawn_mock_oidc_provider().await;
    let downstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/data"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&downstream)
        .await;

    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    cfg.routes.push(RouteDef {
        path: "/api/data".into(),
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

    // 注入已过期令牌（expires_in 为负）
    let tokens = StoredTokens::new(
        "mock",
        "user-1",
        "old-access-token",
        Some("mock-refresh-token"),
        None,
        -120,
    )
    .unwrap();
    let cookie = common::create_session_with_tokens(&state, &tokens).await;

    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    // 并发 5 个请求
    let mut handles = Vec::new();
    for _ in 0..5 {
        let client = client.clone();
        let url = format!("{}/api/data/data", bff);
        let cookie = cookie.clone();
        handles.push(tokio::spawn(async move {
            client
                .get(url)
                .header("cookie", cookie)
                .send()
                .await
                .unwrap()
                .status()
        }));
    }
    let mut ok = 0;
    for h in handles {
        if h.await.unwrap() == 200 {
            ok += 1;
        }
    }
    assert_eq!(ok, 5, "所有请求应成功");

    // 锁防惊群：refresh 端点只被调用一次
    assert_eq!(
        idp.refresh_count.load(Ordering::SeqCst),
        1,
        "并发下应仅刷新一次"
    );

    // 后续请求使用刷新后的新令牌
    let resp = client
        .get(format!("{}/api/data/data", bff))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let requests = downstream.received_requests().await.unwrap();
    let last = requests.last().unwrap();
    assert_eq!(
        last.headers.get("authorization").unwrap(),
        "Bearer mock-access-token-refreshed",
        "刷新后应使用新令牌"
    );
}

// ============================================================
// 5. 代理层：上游 401 → 刷新 → 重试成功
// ============================================================

#[tokio::test]
async fn test_proxy_401_triggers_refresh_and_retry() {
    let idp = common::spawn_mock_oidc_provider().await;
    let downstream = MockServer::start().await;

    // 旧 token → 401
    Mock::given(method("GET"))
        .and(path("/data"))
        .and(header("Authorization", "Bearer old-access-token"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&downstream)
        .await;

    // 刷新后的新 token（或其他任何 token）→ 200
    Mock::given(method("GET"))
        .and(path("/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok-refreshed"))
        .mount(&downstream)
        .await;

    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    cfg.routes.push(RouteDef {
        path: "/api/data".into(),
        methods: vec![],
        description: "test".into(),
        auth_required: true,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(downstream.uri()),
            strip_prefix: true,
            proxy_mode: "http".into(),
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    });
    let state = common::make_state(cfg);

    // Token 有效（不在 skew 窗口），但上游拒绝此 token
    let tokens = StoredTokens::new(
        "mock", "user-1",
        "old-access-token", Some("mock-refresh-token"), None,
        3600,
    ).unwrap();
    let cookie = common::create_session_with_tokens(&state, &tokens).await;

    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    let resp = client
        .get(format!("{}/api/data/data", bff))
        .header("cookie", &cookie)
        .send().await.unwrap();

    assert_eq!(resp.status(), 200, "401 后刷新重试应成功");
    assert_eq!(resp.text().await.unwrap(), "ok-refreshed");
    assert_eq!(idp.refresh_count.load(Ordering::SeqCst), 1, "应触发一次刷新");
}

// ============================================================
// 6. 代理层：上游 401 → 无 refresh_token → 返回 401
// ============================================================

#[tokio::test]
async fn test_proxy_401_without_refresh_token_returns_401() {
    let downstream = MockServer::start().await;

    // 上游始终返回 401
    Mock::given(method("GET"))
        .and(path("/data"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&downstream)
        .await;

    let mut cfg = common::base_config();
    cfg.routes.push(RouteDef {
        path: "/api/data".into(),
        methods: vec![],
        description: "test".into(),
        auth_required: true,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(downstream.uri()),
            strip_prefix: true,
            proxy_mode: "http".into(),
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    });
    let state = common::make_state(cfg);

    // Token 有效，但无 refresh_token
    let tokens = StoredTokens::new(
        "mock", "user-1",
        "no-refresh-token", None, None,
        3600,
    ).unwrap();
    let cookie = common::create_session_with_tokens(&state, &tokens).await;

    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    let resp = client
        .get(format!("{}/api/data/data", bff))
        .header("cookie", &cookie)
        .send().await.unwrap();

    // 无 refresh_token 无法刷新，直接透传上游 401
    assert_eq!(resp.status(), 401, "无 refresh_token 无法刷新，应透传 401");
}

// ============================================================
// 7. 代理层：统一路由也支持 401 刷新重试
// ============================================================

#[tokio::test]
async fn test_legacy_route_proxy_401_retry() {
    let idp = common::spawn_mock_oidc_provider().await;
    let downstream = MockServer::start().await;

    // 旧 token → 401
    Mock::given(method("GET"))
        .and(path("/data"))
        .and(header("Authorization", "Bearer old-legacy-token"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&downstream)
        .await;

    // 新 token → 200
    Mock::given(method("GET"))
        .and(path("/data"))
        .respond_with(ResponseTemplate::new(200).set_body_string("legacy-ok"))
        .mount(&downstream)
        .await;

    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    cfg.routes.push(RouteDef {
        path: "/api/legacy".into(),
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

    let tokens = StoredTokens::new(
        "mock", "user-1",
        "old-legacy-token", Some("mock-refresh-token"), None,
        3600,
    ).unwrap();
    let cookie = common::create_session_with_tokens(&state, &tokens).await;

    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    let resp = client
        .get(format!("{}/api/legacy/data", bff))
        .header("cookie", &cookie)
        .send().await.unwrap();

    assert_eq!(resp.status(), 200, "legacy 路由 401 刷新重试应成功");
    assert_eq!(resp.text().await.unwrap(), "legacy-ok");
    assert_eq!(idp.refresh_count.load(Ordering::SeqCst), 1);
}
