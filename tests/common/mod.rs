//! 集成测试公共工具：构造 App、mock OIDC Provider、mock 下游服务。
#![allow(dead_code)]

use bff::config::{
    AdminConfig, AppConfig, InputMapping, LogoutScope, OidcProviderConfig, OidcSection,
    OutputMapping, ProviderConfig, RouteDef, RouteType, RouteTypeConfig, ServerConfig,
    SessionConfig, SiteConfig, SiteOidcConfig, SpaConfig, TokenRefreshConfig,
};
use bff::site::SiteHandle;
use bff::state::AppState;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tower_sessions::Session;

/// 基础测试配置（全内存 provider）。
pub fn base_config() -> AppConfig {
    AppConfig {
        server: ServerConfig {
            business_port: 0,
            admin_port: 0,
            ..Default::default()
        },
        provider: ProviderConfig::default(),
        session: SessionConfig::default(),
        admin: AdminConfig {
            ip_whitelist: vec!["127.0.0.1".into()],
            auth_mode: "token".into(),
            auth_token: "test-admin-token".into(),
            enable_test_endpoints: true,
            test_endpoint_rate_limit: 10,
            ..Default::default()
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

/// 按站点名启动业务端口（绑定随机端口），返回 base URL（§6.1 多站点夹具）。
pub async fn spawn_site(state: &AppState, name: &str) -> String {
    let handle = site_handle(state, name);
    let router =
        bff::server::business::build_site_router(state.clone(), handle).expect("构建站点路由失败");
    spawn(router).await
}

/// 按站点名取启动期构建的站点句柄（§6.1；不存在则 panic）。
pub fn site_handle(state: &AppState, name: &str) -> Arc<SiteHandle> {
    state
        .site_handles()
        .expect("构建站点句柄失败")
        .into_iter()
        .find(|h| h.name == name)
        .unwrap_or_else(|| panic!("站点 [{name}] 句柄不存在"))
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
///
/// 必须 `no_proxy()`——否则会继承环境 `HTTP_PROXY/HTTPS_PROXY`，
/// 导致对 127.0.0.1 的测试请求被代理拦截（结果随环境翻转）。
pub fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
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
    /// token 端点对 authorization_code grant 返回的 access_token
    /// （默认 `mock-access-token`；令牌隔离用例改写为站点可区分的值）
    pub access_token: Arc<Mutex<String>>,
}

#[derive(Clone)]
struct IdpState {
    url: String,
    nonce: Arc<Mutex<Option<String>>>,
    refresh_count: Arc<AtomicUsize>,
    access_token: Arc<Mutex<String>>,
}

pub async fn spawn_mock_oidc_provider() -> MockIdp {
    spawn_mock_oidc_provider_with_token("mock-access-token").await
}

/// 指定 access_token 的 mock IdP（默认值保持既有测试的期望不变）。
pub async fn spawn_mock_oidc_provider_with_token(access_token: &str) -> MockIdp {
    use axum::{extract::State as AxState, routing::get, routing::post, Json, Router};

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let st = IdpState {
        url: url.clone(),
        nonce: Arc::new(Mutex::new(None)),
        refresh_count: Arc::new(AtomicUsize::new(0)),
        access_token: Arc::new(Mutex::new(access_token.to_string())),
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
                "access_token": st.access_token.lock().unwrap().clone(),
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
        access_token: st.access_token,
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
        shared_across_sites: false,
    }
}

/// 多站点 dev 语义夹具（§5.1/§5.2）：两站点共享 `default` profile 的 Domain cookie
/// （`BFF_SESSION_V2` / `.test` / `allow_unmanaged_subdomains: true`），各自绑定一个
/// mock provider（`shared_across_sites: false`）。
///
/// `server_names: []` + `public_base_url: None` = dev 语义（loopback Host 兜底）；
/// 需要 prod 语义（public_base_url / 421 / Host 白名单）的用例应基于 `base_config()`
/// 自行构造并显式设置站点字段。
pub fn multisite_config(idp_a: &MockIdp, idp_b: &MockIdp) -> AppConfig {
    let mut cfg = base_config();
    // 全局校验仍要求 business_port > 0 且 ≠ admin_port（多站点下该端口仅占位）。
    cfg.server.business_port = 8080;
    cfg.server.admin_port = 8443;
    cfg.session = SessionConfig {
        cookie_name: "BFF_SESSION_V2".into(),
        cookie_domain: Some(".test".into()),
        allow_unmanaged_subdomains: true,
        ..Default::default()
    };
    cfg.sites = vec![site_cfg("app1", 8081, "pA"), site_cfg("app2", 8082, "pB")];
    cfg.oidc.providers = vec![
        multisite_provider_cfg(idp_a, "pA"),
        multisite_provider_cfg(idp_b, "pB"),
    ];
    cfg
}

/// 站点限定 Static 路由（§5.3）：`sites` 过滤，用于验证 per-site 路由隔离。
///
/// 响应体为 JSON `{ "ok": true, "site": <站点名> }`，命中时返回 200。
pub fn route_with_site(path: &str, site: &str) -> RouteDef {
    RouteDef {
        sites: vec![site.into()],
        path: path.into(),
        methods: vec![],
        description: String::new(),
        auth_required: false,
        route_type: RouteType::Static,
        config: RouteTypeConfig {
            body: Some(serde_json::json!({ "ok": true, "site": site })),
            ..Default::default()
        },
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    }
}

fn site_cfg(name: &str, port: u16, provider: &str) -> SiteConfig {
    SiteConfig {
        name: name.into(),
        port,
        bind: "0.0.0.0".into(),
        server_names: vec![],
        public_base_url: None,
        session_profile: "default".into(),
        spa: None,
        oidc: SiteOidcConfig {
            default_provider: provider.into(),
            allowed_providers: Some(vec![provider.into()]),
        },
        logout_scope: LogoutScope::Global,
        security_headers: None,
    }
}

fn multisite_provider_cfg(idp: &MockIdp, id: &str) -> OidcProviderConfig {
    OidcProviderConfig {
        id: id.into(),
        display_name: format!("Mock {id}"),
        ..mock_provider_cfg(idp)
    }
}

/// 合成 provider 配置（不发起任何真实 OIDC 调用）：
/// 供仅需“已登录会话”（`login_cookie` / `create_session_with_tokens`）的测试声明
/// provider 白名单——站点化后 current_provider 必须 ∈ 站点 allowed_providers（§7.2）。
pub fn synthetic_provider_cfg() -> OidcProviderConfig {
    OidcProviderConfig {
        id: "mock".into(),
        display_name: "Mock".into(),
        issuer_url: "http://127.0.0.1:1".into(),
        client_id: "client".into(),
        client_secret: String::new(),
        callback_path: "/auth/callback".into(),
        scopes: vec!["openid".into()],
        insecure_skip_id_token_verification: true,
        refresh_skew_secs: 60,
        shared_across_sites: false,
    }
}

/// 用 `state` 的会话存储构造 `Session`：`None` 新建；`Some(id)` 复现已保存会话
/// （同一 store、同一 id —— 用于验证跨站点视图的令牌可见性，§7.1）。
pub fn session_for(state: &AppState, id: Option<tower_sessions::session::Id>) -> Session {
    Session::new(
        id,
        Arc::new(bff::provider::session::DynSessionStore::new(
            state.session_store.clone(),
        )),
        None,
    )
}

/// 把令牌写入会话的 `oidc:{provider}:tokens` 键（§7.2 命名空间）。
async fn insert_tokens(session: &Session, tokens: &bff::oidc::StoredTokens) {
    session
        .insert(&bff::oidc::tokens::session_key(&tokens.provider), tokens)
        .await
        .unwrap();
}

/// 构造并写入指定 provider 的标准测试令牌，返回该令牌（调用方自行 `save()`）。
pub async fn write_tokens(session: &Session, provider: &str) -> bff::oidc::StoredTokens {
    let tokens = bff::oidc::StoredTokens::new(
        provider,
        "test-user",
        "test-access-token",
        Some("test-refresh-token"),
        None,
        3600,
    )
    .expect("构造 StoredTokens 失败");
    insert_tokens(session, &tokens).await;
    tokens
}

/// 直接在 Session store 中写入令牌，返回 Cookie 头值。
pub async fn create_session_with_tokens(
    state: &AppState,
    tokens: &bff::oidc::StoredTokens,
) -> String {
    let session = session_for(state, None);
    insert_tokens(&session, tokens).await;
    session
        .insert("oidc:current_provider", &tokens.provider)
        .await
        .unwrap();
    session.save().await.unwrap();
    let id = session.id().expect("session 应有 id");
    format!("BFF_SESSION={}", id)
}

/// 便捷函数：为测试构造一个「已登录」会话，返回 Cookie 头值。
///
/// `/pipeline/:name` 强制要求认证，测试需携带该 Cookie。
pub async fn login_cookie(state: &AppState) -> String {
    let tokens = bff::oidc::StoredTokens::new(
        "mock",
        "test-user",
        "test-access-token",
        Some("test-refresh-token"),
        None,
        3600,
    )
    .expect("构造 StoredTokens 失败");
    create_session_with_tokens(state, &tokens).await
}

/// SPA 夹具目录序号：同一进程内每次调用生成唯一目录。
///
/// 必须唯一：多个 `#[tokio::test]` 并行共享同一进程、同一组临时文件；
/// 若目录同名，某个测试重写 `index.html`/`app.js`（fs::write 先截断再写）
/// 会截断另一个测试仍在 serve 的文件，导致响应体 `UnexpectedEof` /
/// `IncompleteMessage`（CI 上的间歇性失败）。
static SPA_DIR_SEQ: AtomicUsize = AtomicUsize::new(0);

/// 构造临时 SPA 目录（进程内唯一），返回路径。
pub fn make_spa_dir(tag: &str) -> String {
    let seq = SPA_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "bff-test-spa-{}-{}-{}",
        tag,
        std::process::id(),
        seq
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("index.html"),
        "<!DOCTYPE html><html><body>spa</body></html>",
    )
    .unwrap();
    std::fs::write(dir.join("app.js"), "console.log(1);").unwrap();
    dir.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// OIDC 登录辅助（跨站点用例共用：SSO §7.3 与登出 §7.4）
// ---------------------------------------------------------------------------

/// 会话 Cookie 名（`multisite_config` 的 `default` profile）。
pub const SESSION_COOKIE: &str = "BFF_SESSION_V2";

/// 从响应 set-cookie 提取 `BFF_SESSION_V2=<id>`（Domain cookie 需显式转发，§7.1）。
pub fn extract_session_cookie(resp: &reqwest::Response) -> Option<String> {
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
pub async fn login_on(
    client: &reqwest::Client,
    idp: &MockIdp,
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
