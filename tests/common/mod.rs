//! 集成测试公共工具：构造 App、mock OIDC Provider、mock 下游服务。
#![allow(dead_code)]

use bff::config::{
    AdminConfig, AppConfig, OidcProviderConfig, OidcSection, ProviderConfig, ServerConfig,
    SessionConfig, SpaConfig, TokenRefreshConfig,
};
use bff::state::AppState;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// 基础测试配置（全内存 provider）。
pub fn base_config() -> AppConfig {
    AppConfig {
        server: ServerConfig {
            business_port: 0,
            admin_port: 0,
        },
        provider: ProviderConfig::default(),
        session: SessionConfig::default(),
        admin: AdminConfig {
            ip_whitelist: vec!["127.0.0.1".into()],
            auth_mode: "token".into(),
            auth_token: "test-admin-token".into(),
            enable_test_endpoints: true,
            test_endpoint_rate_limit: 10,
        },
        spa: SpaConfig {
            dir: "frontend/dist".into(),
        },
        oidc: OidcSection::default(),
        pipelines: HashMap::new(),
        token_refresh: TokenRefreshConfig::default(),
        routes: vec![],
        ..Default::default()
    }
}

pub fn make_state(mut cfg: AppConfig) -> AppState {
    // 使用非零端口通过校验（实际监听由 spawn 绑定随机端口）
    cfg.server.business_port = 8080;
    cfg.server.admin_port = 8443;
    AppState::new(cfg).expect("构造 AppState 失败")
}

/// 启动业务端口（绑定随机端口），返回 base URL。
pub async fn spawn_business(state: AppState) -> String {
    let router = bff::server::business::build_business_router(state).expect("构建业务路由失败");
    spawn(router).await
}

/// 启动管理端口（绑定随机端口），返回 base URL。
pub async fn spawn_admin(state: AppState) -> String {
    let router = bff::server::admin::build_admin_router(state).expect("构建管理路由失败");
    spawn(router).await
}

async fn spawn(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("绑定端口失败");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    format!("http://{}", addr)
}

/// 带 cookie jar、不自动跟随重定向的测试客户端。
pub fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

// ---------------------------------------------------------------------------
// Mock OIDC Provider
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct MockIdp {
    pub url: String,
    /// 测试在 /login 后写入 nonce，token 端点据此构造 id_token
    pub nonce: Arc<Mutex<Option<String>>>,
    /// refresh_token grant 的调用次数
    pub refresh_count: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct IdpState {
    url: String,
    nonce: Arc<Mutex<Option<String>>>,
    refresh_count: Arc<AtomicUsize>,
}

pub async fn spawn_mock_oidc_provider() -> MockIdp {
    use axum::{extract::State as AxState, routing::get, routing::post, Json, Router};

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let st = IdpState {
        url: url.clone(),
        nonce: Arc::new(Mutex::new(None)),
        refresh_count: Arc::new(AtomicUsize::new(0)),
    };

    async fn discovery(AxState(st): AxState<IdpState>) -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "issuer": st.url,
            "authorization_endpoint": format!("{}/authorize", st.url),
            "token_endpoint": format!("{}/token", st.url),
            "jwks_uri": format!("{}/jwks", st.url),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
        }))
    }

    fn make_id_token(st: &IdpState) -> String {
        use base64::Engine;
        let b64 = |v: serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v).unwrap())
        };
        let header = b64(serde_json::json!({"alg": "none", "typ": "JWT"}));
        let exp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        let nonce = st.nonce.lock().unwrap().clone().unwrap_or_default();
        let payload = b64(serde_json::json!({
            "sub": "user-1",
            "iss": st.url,
            "aud": "bff-client",
            "exp": exp,
            "iat": exp - 3600,
            "nonce": nonce,
        }));
        format!("{}.{}.", header, payload)
    }

    async fn token(
        AxState(st): AxState<IdpState>,
        axum::Form(form): axum::Form<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        let grant = form.get("grant_type").cloned().unwrap_or_default();
        match grant.as_str() {
            "authorization_code" => Json(serde_json::json!({
                "access_token": "mock-access-token",
                "token_type": "Bearer",
                "expires_in": 3600,
                "refresh_token": "mock-refresh-token",
                "id_token": make_id_token(&st),
            })),
            "refresh_token" => {
                st.refresh_count.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({
                    "access_token": "mock-access-token-refreshed",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "refresh_token": "mock-refresh-token-2",
                    "id_token": make_id_token(&st),
                }))
            }
            other => Json(serde_json::json!({
                "error": "unsupported_grant_type",
                "error_description": other,
            })),
        }
    }

    async fn jwks() -> Json<serde_json::Value> {
        Json(serde_json::json!({"keys": []}))
    }

    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/token", post(token))
        .route("/jwks", get(jwks))
        .with_state(st.clone());

    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    MockIdp {
        url,
        nonce: st.nonce,
        refresh_count: st.refresh_count,
    }
}

/// 指向 mock IdP 的 provider 配置（跳过验签，仅测试用）。
pub fn mock_provider_cfg(idp: &MockIdp) -> OidcProviderConfig {
    OidcProviderConfig {
        id: "mock".into(),
        display_name: "Mock IdP".into(),
        issuer_url: idp.url.clone(),
        client_id: "bff-client".into(),
        client_secret: "bff-secret".into(),
        callback_path: "/auth/callback".into(),
        scopes: vec!["openid".into()],
        insecure_skip_id_token_verification: true,
        refresh_skew_secs: 60,
    }
}

/// 直接在 Session store 中写入令牌，返回 Cookie 头值。
pub async fn create_session_with_tokens(
    state: &AppState,
    tokens: &bff::oidc::StoredTokens,
) -> String {
    use tower_sessions::Session;
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
    let id = session.id().expect("session 应有 id");
    format!("BFF_SESSION={}", id)
}

/// 构造临时 SPA 目录，返回路径。
pub fn make_spa_dir(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("bff-test-spa-{}-{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("index.html"),
        "<!DOCTYPE html><html><body>spa</body></html>",
    )
    .unwrap();
    std::fs::write(dir.join("app.js"), "console.log(1);").unwrap();
    dir.to_string_lossy().into_owned()
}
