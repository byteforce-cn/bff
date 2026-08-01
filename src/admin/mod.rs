//! 管理端：独立端口的配置管理 API + 内嵌管理 UI。
pub mod config_api;
pub mod runtime_api;

use crate::middleware::ip_whitelist::{ip_whitelist_middleware, IpWhitelist};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "admin-ui/dist"]
struct AdminAssets;

pub fn build_admin_router(state: AppState) -> anyhow::Result<Router> {
    let whitelist = IpWhitelist::parse(&state.cfg().admin.ip_whitelist)
        .map_err(|e| anyhow::anyhow!("IP 白名单配置非法: {}", e))?;

    let api_routes = Router::new()
        .route("/health", get(runtime_api::health))
        .route("/metrics", get(runtime_api::metrics))
        .route("/sessions", get(runtime_api::list_sessions))
        .route("/sessions/:id", delete(runtime_api::delete_session))
        .route("/config/export", get(config_api::export_config))
        .route("/config/import", post(config_api::import_config))
        .route("/oidc/providers", get(config_api::list_providers))
        .route("/oidc/providers/:id", put(config_api::update_provider))
        .route(
            "/pipelines",
            get(config_api::list_pipelines).post(config_api::create_pipeline),
        )
        .route("/pipelines/:name", delete(config_api::delete_pipeline))
        .route("/pipelines/:name/test", post(runtime_api::test_pipeline))
        .route("/scripts", get(config_api::list_scripts))
        .route("/scripts/:name", put(config_api::update_script))
        .route("/scripts/:name/eval", post(config_api::eval_script))
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
        // 先认证、后 IP 白名单（后添加的 layer 更靠外，白名单最先执行）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            test_endpoint_guard,
        ))
        .layer(axum::middleware::from_fn(move |req, next| {
            ip_whitelist_middleware(whitelist.clone(), req, next)
        }))
        .with_state(state);
    Ok(router)
}

/// 管理 API 认证：auth_mode=token 时校验 X-Admin-Token / Bearer。
/// 仅保护 /admin/api/* 路径；管理 UI 静态资源（/index.html 等）直接放行。
async fn admin_auth_middleware(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    // 非 API 路径（管理 UI 静态资源 + SPA fallback）不需要 Token
    if !req.uri().path().starts_with("/admin/api/") {
        return next.run(req).await;
    }

    let cfg = state.cfg();
    if cfg.admin.auth_mode == "none" {
        return next.run(req).await;
    }
    let ok = req
        .headers()
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .map(|t| t == cfg.admin.auth_token)
        .unwrap_or(false)
        || req
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|t| t == cfg.admin.auth_token)
            .unwrap_or(false);
    if ok {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "管理 API 未授权"})),
        )
            .into_response()
    }
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
    Some(
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", mime)
            .body(Body::from(content.data.into_owned()))
            .ok()?,
    )
}
