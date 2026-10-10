//! 管理端：独立端口的配置管理 API + 内嵌管理 UI。
pub mod config_api;
pub mod runtime_api;

use crate::middleware::client_ip::resolve_client_ip;
use crate::middleware::ip_whitelist::IpWhitelist;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use rust_embed::RustEmbed;
use std::net::SocketAddr;

#[derive(RustEmbed)]
#[folder = "admin-ui/dist"]
struct AdminAssets;

pub fn build_admin_router(state: AppState) -> anyhow::Result<Router> {
    // 白名单不再闭包捕获，改为每请求从 `state.cfg()` 实时读取
    // （与 auth_token 一致，热导入立即生效）；启动时仅做格式预检，尽早暴露非法配置。
    let _ = IpWhitelist::parse(&state.cfg().admin.ip_whitelist)
        .map_err(|e| anyhow::anyhow!("IP 白名单配置非法: {}", e))?;
    // 管理 API 请求体上限（与业务 body_limit 独立）
    let admin_body_limit = state.cfg().admin.max_body_bytes;

    let api_routes = Router::new()
        .route("/health", get(runtime_api::health))
        .route("/metrics", get(runtime_api::metrics))
        .route("/sessions", get(runtime_api::list_sessions))
        .route("/sessions/{id}", delete(runtime_api::delete_session))
        .route("/sites", get(runtime_api::list_sites))
        .route("/config/export", get(config_api::export_config))
        .route("/config/import", post(config_api::import_config))
        .route("/oidc/providers", get(config_api::list_providers))
        .route(
            "/oidc/providers/{id}",
            put(config_api::update_provider).delete(config_api::delete_provider),
        )
        .route(
            "/oidc/providers/{id}/verify",
            post(runtime_api::verify_provider),
        )
        .route(
            "/pipelines",
            get(config_api::list_pipelines).post(config_api::create_pipeline),
        )
        .route("/pipelines/{name}", delete(config_api::delete_pipeline))
        .route("/pipelines/{name}/test", post(runtime_api::test_pipeline))
        .route("/scripts", get(config_api::list_scripts))
        .route("/scripts/{name}", put(config_api::update_script))
        .route("/scripts/{name}/eval", post(config_api::eval_script))
        .route(
            "/routes",
            get(config_api::list_routes).put(config_api::update_routes),
        )
        .route("/routes/types", get(config_api::list_route_types));

    let router = Router::new()
        // 版本化 API（v1）
        .nest("/admin/api/v1", api_routes.clone())
        // 兼容旧路径（无版本前缀）
        .nest("/admin/api", api_routes)
        .fallback(admin_ui_fallback)
        // 管理 UI 静态资源启用 gzip
        .layer(tower_http::compression::CompressionLayer::new())
        // 请求体上限
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            admin_body_limit,
        ))
        // 管理写操作审计（操作者、来源 IP、状态）
        .layer(axum::middleware::from_fn(admin_audit_middleware))
        // 认证（常量时间比较 + 失败限流）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            test_endpoint_guard,
        ))
        // 白名单实时生效 + 统一客户端 IP 解析
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_ip_whitelist_live,
        ))
        // 管理面安全响应头（原实现只有业务端口有）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_security_headers,
        ))
        .with_state(state);
    Ok(router)
}

/// 常量时间比较（防时序侧信道）。
///
/// 先哈希再比较固定长度摘要，避免长度/前缀差异直接泄漏。
fn constant_time_eq(a: &str, b: &str) -> bool {
    use sha2::{Digest, Sha256};
    let ha = Sha256::digest(a.as_bytes());
    let hb = Sha256::digest(b.as_bytes());
    let mut diff = 0u8;
    for (x, y) in ha.iter().zip(hb.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 提取请求携带的管理 token（X-Admin-Token 或 Bearer）。
fn extract_admin_token(req: &Request<Body>) -> Option<&str> {
    req.headers()
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            req.headers()
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
        })
}

/// 管理 API 认证：auth_mode=token 时校验 X-Admin-Token / Bearer。
/// 仅保护 /admin/api/* 路径；管理 UI 静态资源（/index.html 等）直接放行。
///
/// 常量时间比较 + 失败次数限流（每来源 IP 每分钟）。
async fn admin_auth_middleware(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    // 非 API 路径（管理 UI 静态资源 + SPA fallback）不需要 Token
    if !req.uri().path().starts_with("/admin/api/") {
        return next.run(req).await;
    }

    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    let (auth_mode, expected) = {
        let cfg = state.cfg();
        (cfg.admin.auth_mode.clone(), cfg.admin.auth_token.clone())
    };
    if auth_mode == "none" {
        return next.run(req).await;
    }

    let ok = extract_admin_token(&req)
        .map(|t| constant_time_eq(t, &expected))
        .unwrap_or(false);
    if ok {
        return next.run(req).await;
    }

    // 失败限流：按来源 IP 计数，超限后 429（auth_mode=none 时不受影响）
    let ip = resolve_client_ip(req.headers(), peer, state.cfg().admin.trusted_proxies);
    if let Some(ip) = ip {
        let limit = state.cfg().admin.auth_fail_limit_per_minute;
        if limit > 0 {
            let key = format!("bff:admin:authfail:{}", ip);
            let current: u32 = state
                .cache
                .get(&key)
                .await
                .and_then(|v| String::from_utf8(v).ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let next_count = current.saturating_add(1);
            state
                .cache
                .set(
                    &key,
                    next_count.to_string().into_bytes(),
                    std::time::Duration::from_secs(60),
                )
                .await;
            if next_count > limit {
                tracing::warn!(%ip, count = next_count, "管理 API 认证失败次数超限，返回 429");
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    axum::Json(serde_json::json!({"error": "too_many_requests"})),
                )
                    .into_response();
            }
        }
    }

    (
        StatusCode::UNAUTHORIZED,
        axum::Json(serde_json::json!({"error": "管理 API 未授权"})),
    )
        .into_response()
}

/// 管理端口 IP 白名单（每请求实时读取配置）。
async fn admin_ip_whitelist_live(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let (entries, trusted) = {
        let cfg = state.cfg();
        (cfg.admin.ip_whitelist.clone(), cfg.admin.trusted_proxies)
    };
    let whitelist = match IpWhitelist::parse(&entries) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = %e, "IP 白名单配置非法，拒绝全部管理请求");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": "IP 白名单配置非法"})),
            )
                .into_response();
        }
    };
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    let ip = resolve_client_ip(req.headers(), peer, trusted);
    match ip {
        Some(ip) if whitelist.allows(&ip) => next.run(req).await,
        Some(ip) => {
            tracing::warn!(%ip, "管理端口拒绝非白名单 IP");
            (
                StatusCode::FORBIDDEN,
                axum::Json(serde_json::json!({"error": "Forbidden: IP 不在白名单"})),
            )
                .into_response()
        }
        None => (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({"error": "Forbidden: 无法确定来源 IP"})),
        )
            .into_response(),
    }
}

/// 管理面安全响应头（管理 UI 为自托管 SPA，CSP 保持同源）。
async fn admin_security_headers(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let hsts = state.cfg().security_headers.hsts_max_age;
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        axum::http::HeaderName::from_static("content-security-policy"),
        axum::http::HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data:; connect-src 'self'; object-src 'none'; \
             base-uri 'self'; frame-ancestors 'none'",
        ),
    );
    h.insert(
        axum::http::HeaderName::from_static("x-frame-options"),
        axum::http::HeaderValue::from_static("DENY"),
    );
    h.insert(
        axum::http::HeaderName::from_static("x-content-type-options"),
        axum::http::HeaderValue::from_static("nosniff"),
    );
    h.insert(
        axum::http::HeaderName::from_static("referrer-policy"),
        axum::http::HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    if hsts > 0 {
        if let Ok(v) = format!("max-age={}", hsts).parse() {
            h.insert(
                axum::http::HeaderName::from_static("strict-transport-security"),
                v,
            );
        }
    }
    resp
}

/// 管理写操作审计（操作者标识、来源 IP、路径、结果状态）。
///
/// 当前管理面只有单一 token 身份，故 `actor = "admin-token"`；
/// 变更 diff 由各写接口在应用配置后输出（见 `config_api::audit_diff`）。
async fn admin_audit_middleware(req: Request<Body>, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "-".into());
    let resp = next.run(req).await;
    if method != axum::http::Method::GET && path.starts_with("/admin/api/") {
        tracing::info!(
            event = "admin.audit",
            actor = "admin-token",
            %ip,
            %method,
            %path,
            status = resp.status().as_u16(),
            "管理操作"
        );
    }
    resp
}

/// test/eval 端点守卫：enable_test_endpoints=false 时返回 403。
async fn test_endpoint_guard(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    let is_test_endpoint = path.ends_with("/eval") || path.ends_with("/test");

    if is_test_endpoint {
        let cfg = state.cfg();
        if !cfg.admin.enable_test_endpoints {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "test/eval 端点已禁用（admin.enable_test_endpoints = false）"})),
            )
                .into_response();
        }
    }

    next.run(req).await
}

/// 管理 UI：内嵌静态资源，未命中路径回退 index.html。
async fn admin_ui_fallback(uri: axum::http::Uri) -> Response {
    // 未匹配的 /admin/api/* 返回 404 JSON，而非 200 + 管理 UI HTML
    // （原行为让 API 客户端无法区分“路径写错”与“成功”）
    if uri.path().starts_with("/admin/api/") {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error": "未知管理 API 路径"})),
        )
            .into_response();
    }
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(resp) = serve_embedded(path) {
        return resp;
    }
    if let Some(resp) = serve_embedded("index.html") {
        return resp;
    }
    (
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({"error": "管理 UI 资源不存在"})),
    )
        .into_response()
}

fn serve_embedded(path: &str) -> Option<Response> {
    let content = AdminAssets::get(path)?;
    let mime = match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", mime)
        .body(Body::from(content.data.into_owned()))
        .ok()
}
