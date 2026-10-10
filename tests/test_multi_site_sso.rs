//! Task 13：跨站点 SSO 与令牌隔离集成测试（§7.3 / §8.1 / §14-2/3/4/13）。
//!
//! 两站点共享 `default` profile 的 Domain cookie（`BFF_SESSION_V2` / `.test`）：
//! 会话存储同一份，但令牌按站点白名单隔离。由于 Domain cookie 不会被
//! reqwest 的 jar 回传到 127.0.0.1，测试解析 set-cookie 后以显式 `cookie:` 头
//! 模拟浏览器的域 Cookie 行为（§7.1）。

mod common;

use bff::config::{InputMapping, OutputMapping, RouteDef, RouteType, RouteTypeConfig};
use bff::state::AppState;
use std::collections::HashMap;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 会话 Cookie 名（`multisite_config` 的 `default` profile）。
const SESSION_COOKIE: &str = "BFF_SESSION_V2";

/// 从响应 set-cookie 提取 `BFF_SESSION_V2=<id>`（Domain cookie 需显式转发，§7.1）。
fn extract_session_cookie(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(|c| c.split(';').next().unwrap_or_default().trim().to_string())
        .find(|pair| pair.starts_with(&format!("{SESSION_COOKIE}=")))
}

/// 在站点上完成一次完整 OIDC 登录（§7.3 时序）：
///
/// `GET /login?provider=` → 302 到 IdP authorize；解析 state/nonce 并写入 mock IdP；
/// `GET {callback_path}?code=mock-code&state=…`（携带会话 Cookie）。
/// 返回回调响应中的会话 Cookie 值（`cycle_id` 轮换后的新 id），供后续
/// 跨站点请求显式携带。
async fn login_on(
    client: &reqwest::Client,
    idp: &common::MockIdp,
    base: &str,
    provider: &str,
    cookie: Option<&str>,
) -> String {
    // 1. /login → 302 到 IdP authorize
    let mut req = client
        .get(format!("{base}/login"))
        .query(&[("provider", provider)]);
    if let Some(c) = cookie {
        req = req.header("cookie", c);
    }
    let resp = req.send().await.unwrap();
    assert!(
        resp.status().is_redirection(),
        "/login?provider={provider} 应 3xx，实际: {}",
        resp.status()
    );
    let location = resp
        .headers()
        .get("location")
        .expect("/login 响应应有 location")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        location.starts_with(&format!("{}/authorize", idp.url)),
        "/login 应重定向到 {}/authorize，实际: {location}",
        idp.url
    );
    // /login 会新建/更新会话 Cookie；回调必须携带同一会话
    let cookie = extract_session_cookie(&resp)
        .or_else(|| cookie.map(|c| c.to_string()))
        .expect("login 后应有会话 Cookie");

    // 2. 解析 authorize URL 的 state/nonce，写入 mock IdP（token 端点据此构造 id_token）
    let auth_url = url::Url::parse(&location).unwrap();
    let params: HashMap<_, _> = auth_url.query_pairs().into_owned().collect();
    let state_param = params
        .get("state")
        .expect("authorize URL 应含 state")
        .clone();
    let nonce = params
        .get("nonce")
        .expect("authorize URL 应含 nonce")
        .clone();
    *idp.nonce.lock().unwrap() = Some(nonce);

    // 3. 模拟 IdP 回调（popup=false → 302 重定向回站点）
    let resp = client
        .get(format!("{base}/auth/callback"))
        .query(&[
            ("code", "mock-code"),
            ("state", &state_param),
            ("provider", provider),
        ])
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    if !resp.status().is_redirection() {
        panic!(
            "回调应 3xx（popup=false），实际: {} body: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }
    // cycle_id 轮换后返回新会话 id（缺失时沿用输入 Cookie，防御未来语义变化）
    extract_session_cookie(&resp).unwrap_or(cookie)
}

/// `auth_required` 的 Proxy 路由（`strip_prefix`），供受保护路由断言使用。
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

/// §14-2 / §7.3：站点 A 登录建立共享会话后，站点 B 收到同一 Cookie：
/// `/api/session` 为 `logged_in=false`，受保护路由 401（A 的令牌不得被 B 使用）。
#[tokio::test]
async fn site_a_login_does_not_grant_site_b_access() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let mut cfg = common::multisite_config(&idp_a, &idp_b);
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/data"))
        .and(header("authorization", "Bearer mock-access-token"))
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

    // A 登录 → 共享会话建立（Domain=.test 的 BFF_SESSION_V2）
    let cookie = login_on(&client, &idp_a, &a, "pA", None).await;
    assert!(cookie.starts_with(&format!("{SESSION_COOKIE}=")));

    // A：同一 Cookie 在 A 可用（对照）
    let resp = client
        .get(format!("{a}/api/session"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["logged_in"], true, "站点 A 应已登录: {body}");
    assert_eq!(body["provider"], "pA");
    let resp = client
        .get(format!("{a}/api/protected/data"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "站点 A 受保护路由应 200");

    // B：会话存在，但站点 B 无令牌（§7.3 时序中的 401）→ logged_in=false
    let resp = client
        .get(format!("{b}/api/session"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["logged_in"], false,
        "站点 B 不得复用 A 的登录态: {body}"
    );
    assert!(body["provider"].is_null());
    let resp = client
        .get(format!("{b}/api/protected/data"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "站点 B 受保护路由应 401");

    upstream.verify().await;
}

/// §14-2 / §7.3：站点 B 携带共享 Cookie 走自己的 provider 静默续登，
/// 回调后 B 受保护路由 200，且上游收到 **B** 的 access token。
#[tokio::test]
async fn site_b_silent_login_establishes_own_session() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider_with_token("token-B").await;
    let mut cfg = common::multisite_config(&idp_a, &idp_b);
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

    // A 先登录，建立共享会话
    let cookie = login_on(&client, &idp_a, &a, "pA", None).await;

    // B 静默续登：同一 Cookie 走 B 自己的 provider（§7.3）
    let cookie = login_on(&client, &idp_b, &b, "pB", Some(&cookie)).await;

    // B 的 /api/session 显示 B 已登录
    let resp = client
        .get(format!("{b}/api/session"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["logged_in"], true, "站点 B 登录后应已登录: {body}");
    assert_eq!(body["provider"], "pB");

    // B 受保护路由 200，上游收到 B 的 access token（wiremock 精确匹配）
    let resp = client
        .get(format!("{b}/api/protected/data"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "站点 B 受保护路由应 200");
    upstream.verify().await;
}

/// §8.1 / §14-3（Review Focus #4）：站点 A 端口请求站点 B 的 provider（或不存在
/// 的 provider）→ 400，不回退默认、不重定向，响应体不含 `location`。
#[tokio::test]
async fn cross_site_provider_is_rejected_with_400() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let state = common::make_state(common::multisite_config(&idp_a, &idp_b));
    let a = common::spawn_site(&state, "app1").await;
    let client = common::test_client();

    // 越站 provider（存在于全局列表但不在站点 A 白名单）→ 400
    let resp = client
        .get(format!("{a}/login"))
        .query(&[("provider", "pB")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "越站 provider 应 400，不得回退默认");
    assert!(
        resp.headers().get("location").is_none(),
        "400 响应不得携带 location 头"
    );
    let body = resp.text().await.unwrap();
    assert!(
        !body.to_ascii_lowercase().contains("location"),
        "400 响应体不得含 location: {body}"
    );

    // 不存在的 provider → 同样 400（显式多站点统一 400，§8.1）
    let resp = client
        .get(format!("{a}/login"))
        .query(&[("provider", "nonexistent")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert!(resp.headers().get("location").is_none());
}

/// §14-4：令牌互不可见——同一共享会话中，站点 A 的路由只用 token-A、
/// 站点 B 的路由只用 token-B；任何站点使用对方 token 的上游请求都会
/// 命中负样本 mock 导致断言失败。
#[tokio::test]
async fn tokens_are_isolated_between_sites() {
    let idp_a = common::spawn_mock_oidc_provider_with_token("token-A").await;
    let idp_b = common::spawn_mock_oidc_provider_with_token("token-B").await;
    let mut cfg = common::multisite_config(&idp_a, &idp_b);
    let upstream = MockServer::start().await;
    // 正样本：A 路径只接受 token-A；B 路径只接受 token-B
    Mock::given(method("GET"))
        .and(path("/a-data"))
        .and(header("authorization", "Bearer token-A"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"site": "a"})))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("GET"))
        .and(path("/b-data"))
        .and(header("authorization", "Bearer token-B"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"site": "b"})))
        .expect(1)
        .mount(&upstream)
        .await;
    // 负样本（expect(0)）：任何站点路径收到对方 token 即失败
    Mock::given(method("GET"))
        .and(path("/a-data"))
        .and(header("authorization", "Bearer token-B"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&upstream)
        .await;
    Mock::given(method("GET"))
        .and(path("/b-data"))
        .and(header("authorization", "Bearer token-A"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&upstream)
        .await;
    cfg.routes
        .push(protected_proxy_route("/api/upa", &upstream.uri()));
    cfg.routes
        .push(protected_proxy_route("/api/upb", &upstream.uri()));
    let state = common::make_state(cfg);
    let a = common::spawn_site(&state, "app1").await;
    let b = common::spawn_site(&state, "app2").await;
    let client = common::test_client();

    // 同一共享会话先后在 A、B 登录
    let cookie = login_on(&client, &idp_a, &a, "pA", None).await;
    let cookie = login_on(&client, &idp_b, &b, "pB", Some(&cookie)).await;

    // A 的路由用 token-A、B 的路由用 token-B；各自 200 且负样本零命中
    let resp = client
        .get(format!("{a}/api/upa/a-data"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "站点 A 受保护路由应 200");
    let resp = client
        .get(format!("{b}/api/upb/b-data"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "站点 B 受保护路由应 200");

    upstream.verify().await;
}

/// §5.4 第 10 条 / §14-13：同一 profile 下两个站点绑定同一 provider，
/// 未确认 `shared_across_sites` → 启动失败；显式确认 → 启动成功。
#[tokio::test]
async fn provider_sharing_across_sites_requires_opt_in() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let mut cfg = common::multisite_config(&idp_a, &idp_b);
    // 两站点绑定同一 provider pA 且未确认共享 → 启动失败
    for site in &mut cfg.sites {
        site.oidc.default_provider = "pA".into();
        site.oidc.allowed_providers = Some(vec!["pA".into()]);
    }
    let err = AppState::new(cfg.clone())
        .err()
        .expect("站点间共享 provider 未确认时应拒绝启动");
    assert!(
        err.to_string().contains("shared_across_sites"),
        "错误应提示 shared_across_sites，实际: {err}"
    );

    // 显式确认 shared_across_sites → 启动成功
    cfg.oidc.providers[0].shared_across_sites = true;
    let state = AppState::new(cfg).expect("确认共享后应启动成功");
    let handles = state.site_handles().expect("构建站点句柄");
    assert_eq!(handles.len(), 2);
}
