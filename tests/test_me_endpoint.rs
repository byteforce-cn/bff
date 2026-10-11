//! `GET /api/me` 身份与会话视图（issue #94）：不泄露令牌、未登录不泄露用户信息。
mod common;

use bff::state::AppState;

/// 由测试直接构造的未签名 ID Token（`alg: none`；`/api/me` 只解码本地 claim，不验签）。
fn unsigned_id_token(claims: serde_json::Value) -> String {
    use base64::Engine;
    let b64 = |v: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v).unwrap())
    };
    format!(
        "{}.{}.",
        b64(serde_json::json!({"alg": "none", "typ": "JWT"})),
        b64(claims)
    )
}

/// 写入一个带身份 claims 的登录会话（`sub` 取自 claims，缺省 `user-1`），返回 Cookie 头值。
async fn login_with_claims(state: &AppState, claims: serde_json::Value) -> String {
    let sub = claims["sub"].as_str().unwrap_or("user-1").to_string();
    login_with_sub(state, &sub, claims).await
}

/// 指定 `sub` 的登录会话（权限缓存隔离用例需要两个不同用户）。
async fn login_with_sub(state: &AppState, sub: &str, claims: serde_json::Value) -> String {
    let id_token = unsigned_id_token(claims);
    let tokens = bff::oidc::StoredTokens::new(
        "mock",
        sub,
        "test-access-token",
        None,
        Some(&id_token),
        3600,
    )
    .expect("构造 StoredTokens 失败");
    common::create_session_with_tokens(state, &tokens).await
}

fn legacy_state_with_provider() -> AppState {
    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::synthetic_provider_cfg());
    common::make_state(cfg)
}

#[tokio::test]
async fn me_reports_logged_out_without_session() {
    let state = legacy_state_with_provider();
    let bff = common::spawn_business(state).await;
    let resp = common::test_client()
        .get(format!("{bff}/api/me"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    // 精确相等：未登录响应不得夹带任何身份字段
    assert_eq!(
        body,
        serde_json::json!({"logged_in": false, "provider": null})
    );
}

#[tokio::test]
async fn me_returns_identity_and_session_when_logged_in() {
    let state = legacy_state_with_provider();
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(
        &state,
        serde_json::json!({
            "sub": "user-1",
            "preferred_username": "alice",
            "name": "Alice",
            "email": "alice@example.test"
        }),
    )
    .await;
    let resp = common::test_client()
        .get(format!("{bff}/api/me"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["logged_in"], true);
    assert_eq!(body["provider"], "mock");
    assert_eq!(body["sub"], "user-1");
    assert_eq!(body["preferred_username"], "alice");
    assert_eq!(body["name"], "Alice");
    assert_eq!(body["email"], "alice@example.test");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(body["expires_at"].as_i64().unwrap() > now);
    let raw = serde_json::to_string(&body).unwrap();
    for needle in [
        "access_token",
        "refresh_token",
        "id_token",
        "test-access-token",
    ] {
        assert!(!raw.contains(needle), "响应不得含 {needle}: {raw}");
    }
}

#[tokio::test]
async fn me_falls_back_to_session_sub_without_id_token() {
    let state = legacy_state_with_provider();
    let bff = common::spawn_business(state.clone()).await;
    // login_cookie 写入的 StoredTokens 无 id_token（无 openid scope 场景）
    let cookie = common::login_cookie(&state).await;
    let resp = common::test_client()
        .get(format!("{bff}/api/me"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["logged_in"], true);
    assert_eq!(body["sub"], "test-user");
    assert!(body.get("name").is_none());
    assert!(body.get("email").is_none());
}
