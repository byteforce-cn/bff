//! 本地 HTTPS E2E 用 Mock OIDC Provider（仅开发/验收演示，切勿用于生产）。
//!
//! 用途：在无 Java/真实 IdP 的环境下，验证 BFF 的完整 OIDC 链路：
//! 登录 → 回调 → 会话 → 刷新 → 登出（走 discovery 的 end_session_endpoint）。
//!
//! 行为：
//! - `/.well-known/openid-configuration`：issuer 与本服务地址一致（使用环境变量
//!   `MOCK_IDP_ISSUER` 指定对外地址，容器内监听 0.0.0.0:9090）；
//! - `/authorize`：不发真实授权页，直接把 `code` 回跳给客户端（保存 nonce）；
//! - `/token`：authorization_code 返回 access/refresh/id_token（id_token 为
//!   未签名 JWT，配套 provider 需开启 insecure_skip_id_token_verification）；
//!   refresh_token grant 返回轮换后的令牌；
//! - `/end_session`：RP-Initiated Logout，回跳 `post_logout_redirect_uri`。
//!
//! 运行：`MOCK_IDP_ISSUER=http://host.docker.internal:9090 cargo run --release --example mock_idp`

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct AppS {
    issuer: String,
    /// code → nonce（authorize 时记录，token 时写入 id_token）
    flows: Arc<Mutex<HashMap<String, String>>>,
    refresh_count: Arc<Mutex<u64>>,
}

#[derive(Deserialize)]
struct AuthorizeParams {
    redirect_uri: String,
    state: Option<String>,
    nonce: Option<String>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let issuer =
        std::env::var("MOCK_IDP_ISSUER").unwrap_or_else(|_| "http://127.0.0.1:9090".into());
    let state = AppS {
        issuer: issuer.clone(),
        flows: Arc::new(Mutex::new(HashMap::new())),
        refresh_count: Arc::new(Mutex::new(0)),
    };

    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/authorize", get(authorize))
        .route("/token", axum::routing::post(token))
        .route("/jwks", get(jwks))
        .route("/end_session", get(end_session))
        .route("/live", get(|| async { "ok" }))
        .with_state(state);

    let addr: std::net::SocketAddr = ([0, 0, 0, 0], 9090).into();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("绑定 9090 失败");
    tracing::info!(%issuer, "Mock IdP 监听 0.0.0.0:9090");
    axum::serve(listener, app).await.ok();
}

async fn discovery(State(s): State<AppS>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "issuer": s.issuer,
        "authorization_endpoint": format!("{}/authorize", s.issuer),
        "token_endpoint": format!("{}/token", s.issuer),
        "jwks_uri": format!("{}/jwks", s.issuer),
        "end_session_endpoint": format!("{}/end_session", s.issuer),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["none"],
    }))
}

async fn authorize(
    State(s): State<AppS>,
    Query(p): Query<AuthorizeParams>,
) -> axum::response::Response {
    let code = uuid::Uuid::new_v4().simple().to_string();
    if let Some(nonce) = p.nonce.clone() {
        s.flows.lock().unwrap().insert(code.clone(), nonce);
    }
    let mut url = format!("{}?code={}", p.redirect_uri, code);
    if let Some(state) = p.state {
        url.push_str(&format!("&state={}", urlencoding(&state)));
    }
    tracing::info!(redirect = %url, "Mock IdP: 自动授权回跳");
    Redirect::to(&url).into_response()
}

async fn token(
    State(s): State<AppS>,
    axum::Form(form): axum::Form<HashMap<String, String>>,
) -> axum::response::Response {
    let grant = form.get("grant_type").cloned().unwrap_or_default();
    match grant.as_str() {
        "authorization_code" => {
            let code = form.get("code").cloned().unwrap_or_default();
            let nonce = s.flows.lock().unwrap().remove(&code).unwrap_or_default();
            Json(serde_json::json!({
                "access_token": format!("mock-at-{}", uuid::Uuid::new_v4().simple()),
                "token_type": "Bearer",
                "expires_in": 3600,
                "refresh_token": "mock-refresh-1",
                "id_token": make_id_token(&s, &nonce),
            }))
            .into_response()
        }
        "refresh_token" => {
            *s.refresh_count.lock().unwrap() += 1;
            Json(serde_json::json!({
                "access_token": format!("mock-at-refreshed-{}", uuid::Uuid::new_v4().simple()),
                "token_type": "Bearer",
                "expires_in": 3600,
                "refresh_token": "mock-refresh-2",
                "id_token": make_id_token(&s, "refresh"),
            }))
            .into_response()
        }
        other => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "unsupported_grant_type",
                "error_description": other,
            })),
        )
            .into_response(),
    }
}

async fn jwks() -> Json<serde_json::Value> {
    Json(serde_json::json!({"keys": []}))
}

#[derive(Deserialize)]
struct EndSessionParams {
    post_logout_redirect_uri: Option<String>,
}

async fn end_session(Query(p): Query<EndSessionParams>) -> axum::response::Response {
    let target = p.post_logout_redirect_uri.unwrap_or_else(|| "/".into());
    tracing::info!(%target, "Mock IdP: RP-Initiated Logout 回跳");
    Redirect::to(&target).into_response()
}

fn make_id_token(s: &AppS, nonce: &str) -> String {
    use base64::Engine;
    let b64 = |v: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v).unwrap())
    };
    let header = b64(serde_json::json!({"alg": "none", "typ": "JWT"}));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let payload = b64(serde_json::json!({
        "sub": "demo-user",
        "iss": s.issuer,
        "aud": "bff-demo",
        "exp": now + 3600,
        "iat": now,
        "nonce": nonce,
    }));
    format!("{}.{}.", header, payload)
}

fn urlencoding(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}
