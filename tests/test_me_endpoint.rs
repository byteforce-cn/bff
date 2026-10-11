//! `GET /api/me` 身份与会话视图（issue #94）：不泄露令牌、未登录不泄露用户信息。
mod common;

use bff::state::AppState;
use std::time::Duration;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

/// 启用权限视图、指向 mock resolve 服务的配置。
fn state_with_permissions(resolve_url: &str) -> AppState {
    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::synthetic_provider_cfg());
    cfg.me.permissions.enabled = true;
    cfg.me.permissions.resolve_url = resolve_url.to_string();
    common::make_state(cfg)
}

async fn me_body(bff: &str, cookie: &str) -> serde_json::Value {
    let resp = common::test_client()
        .get(format!("{bff}/api/me"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
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

#[tokio::test]
async fn me_returns_permissions_from_authority() {
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/resolve"))
        .and(header("authorization", "Bearer test-access-token"))
        .and(body_json(
            serde_json::json!({"sub": "user-1", "provider": "mock"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "roles": ["admin"],
            "permissions": ["user:read", "user:write"]
        })))
        .expect(1)
        .mount(&resolve)
        .await;
    let state = state_with_permissions(&format!("{}/resolve", resolve.uri()));
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(&state, serde_json::json!({"sub": "user-1"})).await;

    let body = me_body(&bff, &cookie).await;
    assert_eq!(body["logged_in"], true);
    assert_eq!(body["sub"], "user-1");
    assert_eq!(body["permissions"]["roles"], serde_json::json!(["admin"]));
    assert_eq!(
        body["permissions"]["permissions"],
        serde_json::json!(["user:read", "user:write"])
    );
    resolve.verify().await;
}

#[tokio::test]
async fn me_omits_permissions_by_default_and_never_calls_resolve() {
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"roles": ["admin"]})),
        )
        .expect(0)
        .mount(&resolve)
        .await;
    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::synthetic_provider_cfg());
    cfg.me.permissions.resolve_url = format!("{}/resolve", resolve.uri()); // enabled 保持 false
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(&state, serde_json::json!({"sub": "user-1"})).await;

    let body = me_body(&bff, &cookie).await;
    assert!(body.get("permissions").is_none(), "缺省关闭: {body}");
    resolve.verify().await;
}

#[tokio::test]
async fn me_degrades_to_identity_when_resolve_fails() {
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .expect(1)
        .mount(&resolve)
        .await;
    let state = state_with_permissions(&format!("{}/resolve", resolve.uri()));
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(
        &state,
        serde_json::json!({"sub": "user-1", "name": "Alice"}),
    )
    .await;

    let body = me_body(&bff, &cookie).await;
    assert_eq!(body["logged_in"], true);
    assert_eq!(body["name"], "Alice", "身份视图不得被 resolve 失败拖垮");
    assert!(
        body.get("permissions").is_none(),
        "失败时 permissions 不出现: {body}"
    );
    resolve.verify().await;
}

#[tokio::test]
async fn me_degrades_to_identity_when_resolve_times_out() {
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(900))
                .set_body_json(serde_json::json!({"roles": ["admin"]})),
        )
        .expect(1)
        .mount(&resolve)
        .await;
    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::synthetic_provider_cfg());
    cfg.me.permissions.enabled = true;
    cfg.me.permissions.resolve_url = format!("{}/resolve", resolve.uri());
    cfg.me.permissions.timeout = Duration::from_millis(200);
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(&state, serde_json::json!({"sub": "user-1"})).await;

    let body = me_body(&bff, &cookie).await;
    assert_eq!(body["logged_in"], true);
    assert!(body.get("permissions").is_none());
    resolve.verify().await;
}

#[tokio::test]
async fn me_caches_permissions_between_calls() {
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "roles": ["admin"],
            "permissions": ["user:read"]
        })))
        .expect(1) // 第二次调用必须命中缓存
        .mount(&resolve)
        .await;
    let state = state_with_permissions(&format!("{}/resolve", resolve.uri()));
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(&state, serde_json::json!({"sub": "user-1"})).await;

    assert_eq!(
        me_body(&bff, &cookie).await["permissions"]["roles"][0],
        "admin"
    );
    assert_eq!(
        me_body(&bff, &cookie).await["permissions"]["roles"][0],
        "admin"
    );
    resolve.verify().await;
}

/// 权限缓存按 `sub` 隔离：两个用户不得互相命中（键必须含 sub）。
#[tokio::test]
async fn me_permissions_cache_is_per_user() {
    let resolve = MockServer::start().await;
    for (sub, role) in [("user-1", "role-1"), ("user-2", "role-2")] {
        Mock::given(method("POST"))
            .and(body_json(
                serde_json::json!({"sub": sub, "provider": "mock"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "roles": [role],
                "permissions": []
            })))
            .expect(1)
            .mount(&resolve)
            .await;
    }
    let state = state_with_permissions(&format!("{}/resolve", resolve.uri()));
    let bff = common::spawn_business(state.clone()).await;
    let cookie_1 = login_with_sub(&state, "user-1", serde_json::json!({"sub": "user-1"})).await;
    let cookie_2 = login_with_sub(&state, "user-2", serde_json::json!({"sub": "user-2"})).await;

    assert_eq!(
        me_body(&bff, &cookie_1).await["permissions"]["roles"][0],
        "role-1"
    );
    assert_eq!(
        me_body(&bff, &cookie_2).await["permissions"]["roles"][0],
        "role-2"
    );
    resolve.verify().await;
}

/// Review Focus 2：resolve 返回 2xx 但结构非法（roles 非数组）→ 身份照常、permissions 不出现。
#[tokio::test]
async fn me_degrades_when_resolve_shape_is_invalid() {
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"roles": "admin"})),
        )
        .expect(1)
        .mount(&resolve)
        .await;
    let state = state_with_permissions(&format!("{}/resolve", resolve.uri()));
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(&state, serde_json::json!({"sub": "user-1"})).await;

    let body = me_body(&bff, &cookie).await;
    assert_eq!(body["logged_in"], true);
    assert!(
        body.get("permissions").is_none(),
        "结构非法时整体拒绝: {body}"
    );
    resolve.verify().await;
}

/// 站点策略：全局启用 + 站点显式 `false` → 该站点不返回 permissions，也不触发 resolve。
#[tokio::test]
async fn me_site_policy_can_disable_permissions() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "roles": ["admin"],
            "permissions": []
        })))
        .expect(1) // 仅 app1 触发
        .mount(&resolve)
        .await;

    let mut cfg = common::multisite_config(&idp_a, &idp_b);
    cfg.me.permissions.enabled = true;
    cfg.me.permissions.resolve_url = format!("{}/resolve", resolve.uri());
    cfg.sites[1].me = Some(bff::config::SiteMeOverride {
        permissions_enabled: Some(false),
    });
    let state = common::make_state(cfg);
    let a = common::spawn_site(&state, "app1").await;
    let b = common::spawn_site(&state, "app2").await;
    let client = common::test_client();
    // A 先断言：B 的静默续登会轮换共享会话 id（cycle_id），旧 cookie 随即失效
    let cookie_a = common::login_on(&client, &idp_a, &a, "pA", None).await;
    let body_a = me_body(&a, &cookie_a).await;
    assert_eq!(body_a["permissions"]["roles"][0], "admin");
    let cookie_b = common::login_on(&client, &idp_b, &b, "pB", Some(&cookie_a)).await;
    let body_b = me_body(&b, &cookie_b).await;
    assert_eq!(body_b["logged_in"], true);
    assert!(
        body_b.get("permissions").is_none(),
        "站点显式关闭: {body_b}"
    );
    resolve.verify().await;
}

/// Review P1-1：resolve 立即返回响应头、body 永不结束 —— 超时必须覆盖 body 读取，
/// 不能被客户端总超时（默认 30s）拖住。
#[tokio::test]
async fn me_degrades_when_resolve_body_never_finishes() {
    let app = axum::Router::new().route(
        "/resolve",
        axum::routing::post(|| async {
            let stream =
                futures::stream::pending::<Result<axum::body::Bytes, std::convert::Infallible>>();
            (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                axum::body::Body::from_stream(stream),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::synthetic_provider_cfg());
    cfg.me.permissions.enabled = true;
    cfg.me.permissions.resolve_url = format!("http://{addr}/resolve");
    cfg.me.permissions.timeout = Duration::from_millis(200);
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(&state, serde_json::json!({"sub": "user-1"})).await;

    let start = std::time::Instant::now();
    let body = me_body(&bff, &cookie).await;
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "超时必须覆盖 body 读取，实际耗时 {:?}",
        start.elapsed()
    );
    assert_eq!(body["logged_in"], true);
    assert!(body.get("permissions").is_none());
}

/// Review P1-2：resolve 返回超大响应体 → 按上限截断/降级，不得整体缓冲。
#[tokio::test]
async fn me_degrades_when_resolve_body_exceeds_cap() {
    let resolve = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "roles": ["admin"],
            // 约 128 KiB，超过 64 KiB 上限
            "permissions": ["x".repeat(128 * 1024)]
        })))
        .expect(1)
        .mount(&resolve)
        .await;
    let state = state_with_permissions(&format!("{}/resolve", resolve.uri()));
    let bff = common::spawn_business(state.clone()).await;
    let cookie = login_with_claims(&state, serde_json::json!({"sub": "user-1"})).await;

    let body = me_body(&bff, &cookie).await;
    assert_eq!(body["logged_in"], true);
    assert!(
        body.get("permissions").is_none(),
        "超出上限必须降级，不得缓冲整包: {body}"
    );
    resolve.verify().await;
}
