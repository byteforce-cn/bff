//! 回调 provider 解析（§5.4 第 8 条 / §8.1）：真实 IdP 只按注册的 `redirect_uri`
//! 回跳（路径 = `provider.callback_path`），不回带 `?provider=`。站点绑定多个 provider
//! （`callback_path` 唯一）时，必须按匹配到的回调路径选中发起登录的 provider，
//! 否则非默认 provider 的登录永远查不到 flow（401）。
mod common;

use std::collections::HashMap;

/// 发起一次登录（可选 `?provider=`），返回 (state 参数, 会话 Cookie)。
/// mock IdP 的 nonce 同步写入，供 token 端点构造 id_token。
async fn start_login(
    client: &reqwest::Client,
    idp: &common::MockIdp,
    base: &str,
    provider: Option<&str>,
) -> (String, String) {
    let mut req = client.get(format!("{base}/login"));
    if let Some(p) = provider {
        req = req.query(&[("provider", p)]);
    }
    let resp = req.send().await.unwrap();
    assert!(resp.status().is_redirection(), "/login 应 3xx");
    let location = resp.headers()["location"].to_str().unwrap().to_string();
    let cookie = common::extract_session_cookie(&resp).expect("login 后应有会话 Cookie");
    let auth_url = url::Url::parse(&location).unwrap();
    let params: HashMap<_, _> = auth_url.query_pairs().into_owned().collect();
    let state = params
        .get("state")
        .expect("authorize URL 应含 state")
        .clone();
    let nonce = params
        .get("nonce")
        .expect("authorize URL 应含 nonce")
        .clone();
    *idp.nonce.lock().unwrap() = Some(nonce);
    (state, cookie)
}

/// 单站点绑定两个 provider，回调路径不同（§5.4 第 8 条允许）。
fn two_providers_single_site(
    idp_a: &common::MockIdp,
    idp_b: &common::MockIdp,
) -> bff::config::AppConfig {
    let mut cfg = common::multisite_config(idp_a, idp_b);
    cfg.sites.truncate(1);
    cfg.sites[0].oidc.allowed_providers = Some(vec!["pA".into(), "pB".into()]);
    cfg.sites[0].oidc.default_provider = "pA".into();
    cfg.oidc.providers[0].callback_path = "/auth/callback-a".into();
    cfg.oidc.providers[1].callback_path = "/auth/callback-b".into();
    cfg
}

/// 非默认 provider 登录：IdP 回跳到 `/auth/callback-b`（无 `?provider=`）
/// 必须选中 pB，写入 pB 的令牌（`/api/session` 的 provider 依赖令牌存在）。
#[tokio::test]
async fn non_default_provider_callback_resolves_from_matched_path() {
    let idp_a = common::spawn_mock_oidc_provider_with_token("token-A").await;
    let idp_b = common::spawn_mock_oidc_provider_with_token("token-B").await;
    let state = common::make_state(two_providers_single_site(&idp_a, &idp_b));
    let a = common::spawn_site(&state, "app1").await;
    let client = common::test_client();

    let (state_param, cookie) = start_login(&client, &idp_b, &a, Some("pB")).await;
    let resp = client
        .get(format!("{a}/auth/callback-b"))
        .query(&[("code", "mock-code"), ("state", &state_param)])
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status.is_redirection(),
        "pB 回调应 3xx，实际 {status} body {}",
        resp.text().await.unwrap_or_default()
    );
    let cookie = common::extract_session_cookie(&resp).unwrap_or(cookie);

    let resp = client
        .get(format!("{a}/api/session"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["logged_in"], true, "pB 登录后应已登录: {body}");
    assert_eq!(
        body["provider"], "pB",
        "应按回调路径选中发起登录的 pB，而非默认 pA: {body}"
    );
}

/// 默认 provider 登录（无 `?provider=`）行为不变：回跳到 `/auth/callback-a` 选中 pA。
#[tokio::test]
async fn default_provider_callback_still_works() {
    let idp_a = common::spawn_mock_oidc_provider_with_token("token-A").await;
    let idp_b = common::spawn_mock_oidc_provider_with_token("token-B").await;
    let state = common::make_state(two_providers_single_site(&idp_a, &idp_b));
    let a = common::spawn_site(&state, "app1").await;
    let client = common::test_client();

    let (state_param, cookie) = start_login(&client, &idp_a, &a, None).await;
    let resp = client
        .get(format!("{a}/auth/callback-a"))
        .query(&[("code", "mock-code"), ("state", &state_param)])
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status.is_redirection(),
        "默认 provider 回调应 3xx，实际 {status} body {}",
        resp.text().await.unwrap_or_default()
    );
    let cookie = common::extract_session_cookie(&resp).unwrap_or(cookie);

    let resp = client
        .get(format!("{a}/api/session"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["logged_in"], true, "默认 provider 登录应成功: {body}");
    assert_eq!(body["provider"], "pA", "默认 provider 应为 pA: {body}");
}
