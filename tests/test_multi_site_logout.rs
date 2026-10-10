//! Task 14：登出 scope 与 exchange 缓存吊销集成测试（§7.4 / §14-7/14）。
//!
//! 两站点共享 `default` profile 的 Domain cookie（`BFF_SESSION_V2` / `.test`），
//! 同一会话存储承载全部站点令牌；登出 `global` / `site` 的差异与
//! `DELETE /admin/api/sessions/:id` 在共享会话下的“全站剔除”语义在此覆盖。
//! Domain cookie 不会被 reqwest jar 回传到 127.0.0.1，测试一律显式 `cookie:` 头。

mod common;

use bff::config::{
    InputMapping, LogoutScope, OutputMapping, RouteDef, RouteType, RouteTypeConfig,
    TokenExchangeConfig,
};
use std::time::Duration;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// `auth_required` 的 Proxy 路由（`strip_prefix`），用于受保护路由 401/200 断言。
fn protected_proxy_route(path: &str, upstream: &str) -> RouteDef {
    RouteDef {
        sites: vec![],
        path: path.into(),
        methods: vec![],
        description: String::new(),
        auth_required: true,
        route_type: RouteType::Proxy,
        config: RouteTypeConfig {
            upstream: Some(upstream.into()),
            strip_prefix: true,
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    }
}

/// 从 `common::login_on` 返回的 Cookie 提取会话 id。
///
/// `tower_sessions::session::Id` 的 Display = URL_SAFE_NO_PAD base64（无 `;`/`=`），
/// 可直接作为 `DELETE /admin/api/sessions/:id` 的路径参数。
fn session_id(cookie: &str) -> String {
    cookie
        .strip_prefix(&format!("{}=", common::SESSION_COOKIE))
        .expect("Cookie 应以会话 Cookie 名开头")
        .to_string()
}

/// 复刻 `src/server/token_exchange.rs::cache_key` 的键格式
/// （`bff:token_exchange:{session_id}:cfgfp:subfp`，§6.1）。
fn exchange_cache_key(session_id: &str, subject_token: &str) -> String {
    use sha2::{Digest, Sha256};
    let hex = |s: &str| format!("{:x}", Sha256::digest(s.as_bytes()));
    let cfg_fp = &hex(&TokenExchangeConfig::default().fingerprint())[0..16];
    let sub_fp = &hex(subject_token)[0..16];
    format!("bff:token_exchange:{session_id}:{cfg_fp}:{sub_fp}")
}

/// `/api/session` 的 `logged_in`（站点维度登录态）。
async fn logged_in(client: &reqwest::Client, base: &str, cookie: &str) -> bool {
    let resp = client
        .get(format!("{base}/api/session"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    body["logged_in"].as_bool().unwrap_or(false)
}

/// 受保护代理路由的响应码（无令牌 → 401）。
async fn protected_status(
    client: &reqwest::Client,
    base: &str,
    cookie: &str,
) -> reqwest::StatusCode {
    client
        .get(format!("{base}/api/protected/data"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .status()
}

/// §7.4 / §14-7：站点 A 触发默认 `global` 登出后，共享会话被清除，
/// A、B 均登出（受保护路由 401）；登出响应为 302（到 IdP 或 `/` 均可，不得 400/500）。
#[tokio::test]
async fn global_logout_clears_all_sites() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let mut cfg = common::multisite_config(&idp_a, &idp_b);
    // 默认即 global
    assert_eq!(cfg.sites[0].logout_scope, LogoutScope::Global);
    let upstream = MockServer::start().await;
    cfg.routes
        .push(protected_proxy_route("/api/protected", &upstream.uri()));
    let state = common::make_state(cfg);
    let a = common::spawn_site(&state, "app1").await;
    let b = common::spawn_site(&state, "app2").await;
    let client = common::test_client();

    // A、B 先后在同一共享会话内登录
    let cookie = common::login_on(&client, &idp_a, &a, "pA", None).await;
    let cookie = common::login_on(&client, &idp_b, &b, "pB", Some(&cookie)).await;
    assert!(logged_in(&client, &a, &cookie).await, "前置：A 应已登录");
    assert!(logged_in(&client, &b, &cookie).await, "前置：B 应已登录");

    // A 触发 global 登出
    let resp = client
        .get(format!("{a}/logout"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert!(
        !resp.status().is_client_error() && !resp.status().is_server_error(),
        "global 登出不得 4xx/5xx，实际: {}",
        resp.status()
    );
    assert!(
        resp.status().is_redirection(),
        "global 登出应重定向（IdP 或 /），实际: {}",
        resp.status()
    );

    // A、B 均登出且受保护路由 401
    assert!(
        !logged_in(&client, &a, &cookie).await,
        "global 登出后 A 应登出"
    );
    assert!(
        !logged_in(&client, &b, &cookie).await,
        "global 登出后 B 应登出"
    );
    assert_eq!(protected_status(&client, &a, &cookie).await, 401);
    assert_eq!(protected_status(&client, &b, &cookie).await, 401);
}

/// §7.4 / §14-14：站点 A 配置 `site` scope，A 登出仅清除 A 的令牌与 provider 键，
/// 保留会话与站点 B（B 仍 `logged_in=true` 且受保护路由 200），且不触发 IdP（302 `/`）；
/// 同时清理该会话的 exchange 缓存。
#[tokio::test]
async fn site_logout_keeps_other_site_session() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider_with_token("token-B").await;
    let mut cfg = common::multisite_config(&idp_a, &idp_b);
    // app1 = site scope；app2 保持默认 global（不影响本用例的 B 令牌保留）
    cfg.sites[0].logout_scope = LogoutScope::Site;
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/data"))
        .and(header("authorization", "Bearer token-B"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .expect(1)
        .mount(&upstream)
        .await;
    cfg.routes
        .push(protected_proxy_route("/api/protected", &upstream.uri()));
    let state = common::make_state(cfg);
    let a = common::spawn_site(&state, "app1").await;
    let b = common::spawn_site(&state, "app2").await;
    let client = common::test_client();

    let cookie = common::login_on(&client, &idp_a, &a, "pA", None).await;
    let cookie = common::login_on(&client, &idp_b, &b, "pB", Some(&cookie)).await;
    assert!(logged_in(&client, &a, &cookie).await, "前置：A 应已登录");
    assert!(logged_in(&client, &b, &cookie).await, "前置：B 应已登录");

    // 该会话的 exchange 缓存条目（§7.4：site 登出同样清理）
    let key = exchange_cache_key(&session_id(&cookie), "token-B");
    state
        .cache
        .set(&key, b"encrypted-blob".to_vec(), Duration::from_secs(60))
        .await;
    assert!(
        state.cache.get(&key).await.is_some(),
        "前置：缓存条目应存在"
    );

    // A（site scope）登出：重定向 `/`，不触发 IdP。
    // 状态码为 axum `Redirect::to` 的 303（与 global/回调路径一致），
    // 断言只固定“重定向 + location=/”，不绑定具体 3xx 码。
    let resp = client
        .get(format!("{a}/logout"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "site 登出应重定向，实际: {}",
        resp.status()
    );
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "/",
        "site 登出应重定向 `/`"
    );

    // exchange 缓存被清理
    assert!(
        state.cache.get(&key).await.is_none(),
        "site 登出应清理该会话 exchange 缓存"
    );

    // A 登出，B 不受影响
    assert!(
        !logged_in(&client, &a, &cookie).await,
        "site 登出后 A 应登出"
    );
    assert_eq!(protected_status(&client, &a, &cookie).await, 401);
    assert!(
        logged_in(&client, &b, &cookie).await,
        "site 登出后 B 应仍登录"
    );
    assert_eq!(
        protected_status(&client, &b, &cookie).await,
        200,
        "site 登出后 B 受保护路由应 200"
    );
    upstream.verify().await;
}

/// §7.4 / §14-7：`global` 登出吊销该会话的 token exchange 缓存（键前缀
/// `bff:token_exchange:{session_id}:`），防止已登出会话换来的上游令牌在 TTL 内复用。
#[tokio::test]
async fn global_logout_revokes_token_exchange_cache() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let state = common::make_state(common::multisite_config(&idp_a, &idp_b));
    let a = common::spawn_site(&state, "app1").await;
    let client = common::test_client();

    let cookie = common::login_on(&client, &idp_a, &a, "pA", None).await;
    let key = exchange_cache_key(&session_id(&cookie), "mock-access-token");
    state
        .cache
        .set(&key, b"encrypted-token".to_vec(), Duration::from_secs(60))
        .await;
    assert!(
        state.cache.get(&key).await.is_some(),
        "登录后缓存条目应存在"
    );

    let resp = client
        .get(format!("{a}/logout"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "global 登出应重定向，实际: {}",
        resp.status()
    );

    assert!(
        state.cache.get(&key).await.is_none(),
        "global 登出后该会话 exchange 缓存应为 None"
    );
}

/// §7.4 文档化行为：管理端 `DELETE /admin/api/sessions/:id` 在共享会话下等于
/// 全站剔除——调用后 A、B 均失效（受保护路由 401、`logged_in=false`）。
#[tokio::test]
async fn admin_session_delete_evicts_all_sites() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let mut cfg = common::multisite_config(&idp_a, &idp_b);
    let upstream = MockServer::start().await;
    cfg.routes
        .push(protected_proxy_route("/api/protected", &upstream.uri()));
    let state = common::make_state(cfg);
    let a = common::spawn_site(&state, "app1").await;
    let b = common::spawn_site(&state, "app2").await;
    let admin = common::spawn_admin(state.clone()).await;
    let client = common::test_client();

    let cookie = common::login_on(&client, &idp_a, &a, "pA", None).await;
    let cookie = common::login_on(&client, &idp_b, &b, "pB", Some(&cookie)).await;
    assert!(logged_in(&client, &a, &cookie).await, "前置：A 应已登录");
    assert!(logged_in(&client, &b, &cookie).await, "前置：B 应已登录");

    // 管理端按共享会话 id 删除 → 全站剔除
    let resp = client
        .delete(format!(
            "{admin}/admin/api/sessions/{}",
            session_id(&cookie)
        ))
        .header("x-admin-token", "test-admin-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "删除共享会话应 200");

    assert!(!logged_in(&client, &a, &cookie).await, "删除后 A 应登出");
    assert!(!logged_in(&client, &b, &cookie).await, "删除后 B 应登出");
    assert_eq!(protected_status(&client, &a, &cookie).await, 401);
    assert_eq!(protected_status(&client, &b, &cookie).await, 401);
}
