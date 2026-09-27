//! 业务端口（8080）路由：OIDC、统一路由分发、SPA 发布、WebSocket 升级。
use crate::middleware::token_refresh::token_refresh_middleware;
use crate::oidc::handlers as oidc;
use crate::provider::session::build_layer;
use crate::server::route_dispatcher;
use crate::server::tunnel;
use crate::state::AppState;
use crate::utils::AppError;
use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, Query, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use std::collections::HashMap;
use tower::ServiceExt;
use tower_http::cors::CorsLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tower_sessions::Session;

pub fn build_business_router(state: AppState) -> anyhow::Result<Router> {
    let cfg = state.cfg().clone();
    let session_layer = build_layer(state.session_store.clone(), &cfg.session)?;

    // Trace ID: 为每个请求生成 UUID 并传播到响应头
    let request_id_layer = SetRequestIdLayer::new(
        axum::http::HeaderName::from_static("x-request-id"),
        MakeRequestUuid,
    );

    // CORS：S8 默认收紧——仅显式 `permissive: true` 才全开；
    // `allowed_origins` 为空且未开 permissive = 不允许任何跨域来源（原实现回落 permissive）。
    let cors_layer = if cfg.cors.permissive {
        CorsLayer::permissive()
    } else if cfg.cors.allowed_origins.is_empty() {
        CorsLayer::new()
    } else {
        let mut cors = CorsLayer::new();
        for origin in &cfg.cors.allowed_origins {
            cors = cors.allow_origin(
                origin
                    .parse::<axum::http::HeaderValue>()
                    .expect("CORS origin 非法"),
            );
        }
        cors.allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ])
        .allow_credentials(true)
    };

    // 安全响应头中间件
    let sec_headers = cfg.security_headers.clone();

    let mut app = Router::new()
        .route("/login", get(oidc::login))
        .route("/logout", get(oidc::logout))
        .route("/live", get(liveness));

    // F11：按 provider 配置的 `callback_path` 动态注册回调路由（去重），
    // 原实现硬编码 `/auth/callback`——配置改成其它路径时 IdP 回调会落到 SPA fallback，
    // 登录静默失败且无启动期告警。默认入口 `/auth/callback` 始终保留（兼容热添加 provider）。
    {
        let mut callback_paths: Vec<String> = cfg
            .oidc
            .providers
            .iter()
            .map(|p| p.callback_path.clone())
            .collect();
        callback_paths.push("/auth/callback".into());
        callback_paths.sort();
        callback_paths.dedup();
        for path in callback_paths {
            app = app.route(&path, get(oidc::callback));
        }
    }

    let app = app
        .route("/ready", get(readiness))
        .route("/api/session", get(session_info))
        // 兼容旧 /pipeline/:name 路由（内部转为统一 Route 分发）
        .route("/pipeline/:name", get(run_pipeline).post(run_pipeline))
        // WebSocket 升级专用路由（在 fallback 之前匹配）
        .route("/ws", get(ws_upgrade_handler))
        .route("/ws/*rest", get(ws_upgrade_handler))
        .fallback(fallback_handler)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            metrics_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            token_refresh_middleware,
        ))
        // O3：W3C traceparent 注入/传播（入口生成或续接，注入请求头供代理透传上游）
        .layer(axum::middleware::from_fn(
            crate::middleware::trace_context::trace_context_middleware,
        ))
        .layer(session_layer)
        .layer(request_id_layer)
        .layer(PropagateRequestIdLayer::new(
            axum::http::HeaderName::from_static("x-request-id"),
        ))
        .layer(TraceLayer::new_for_http())
        .layer(cors_layer)
        // 全局限流（tower-governor）：与 CSP csp_overrides 同风格按路径前缀收窄，
        // SPA 静态资源等 skip_path_prefixes 命中的请求不消耗全局限流令牌，其余路径保持限流
        .layer(axum::middleware::from_fn_with_state(
            crate::middleware::rate_limit_skip::rate_limit_skip_state(
                cfg.rate_limit.per_second,
                cfg.rate_limit.burst_size,
                cfg.rate_limit.skip_path_prefixes.clone(),
                // S13：全局限流同样按真实客户端 IP 建桶（信任的代理跳数复用认证限流配置）
                cfg.auth_rate_limit.trusted_proxies,
            ),
            crate::middleware::rate_limit_skip::rate_limit_skip_middleware,
        ))
        // 认证端点 per-IP 限流（网络层纵深防御；未启用/未命中路径时原样放行）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::ip_rate_limit::ip_rate_limit_middleware,
        ))
        // 安全响应头
        .layer(axum::middleware::from_fn(
            move |req: axum::http::Request<Body>, next: axum::middleware::Next| {
                let headers = sec_headers.clone();
                async move {
                    let path = req.uri().path().to_string();
                    let mut resp = next.run(req).await;
                    let h = resp.headers_mut();
                    if !headers.content_security_policy.is_empty() {
                        // 按路径前缀细分 CSP：最长前缀命中优先，未命中回退全局
                        let csp = headers
                            .csp_overrides
                            .iter()
                            .filter(|o| path.starts_with(&o.path_prefix))
                            .max_by_key(|o| o.path_prefix.len())
                            .map(|o| o.content_security_policy.as_str())
                            .unwrap_or(headers.content_security_policy.as_str());
                        h.insert(
                            axum::http::HeaderName::from_static("content-security-policy"),
                            csp.parse::<axum::http::HeaderValue>().unwrap(),
                        );
                    }
                    if !headers.x_frame_options.is_empty() {
                        h.insert(
                            axum::http::HeaderName::from_static("x-frame-options"),
                            headers
                                .x_frame_options
                                .parse::<axum::http::HeaderValue>()
                                .unwrap(),
                        );
                    }
                    if !headers.x_content_type_options.is_empty() {
                        h.insert(
                            axum::http::HeaderName::from_static("x-content-type-options"),
                            headers
                                .x_content_type_options
                                .parse::<axum::http::HeaderValue>()
                                .unwrap(),
                        );
                    }
                    if headers.hsts_max_age > 0 {
                        h.insert(
                            axum::http::HeaderName::from_static("strict-transport-security"),
                            format!("max-age={}", headers.hsts_max_age)
                                .parse::<axum::http::HeaderValue>()
                                .unwrap(),
                        );
                    }
                    if !headers.referrer_policy.is_empty() {
                        h.insert(
                            axum::http::HeaderName::from_static("referrer-policy"),
                            headers
                                .referrer_policy
                                .parse::<axum::http::HeaderValue>()
                                .unwrap(),
                        );
                    }
                    resp
                }
            },
        ))
        // 请求体大小限制
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            cfg.body_limit.max_bytes,
        ))
        .with_state(state);
    Ok(app)
}

/// GET /live — K8s liveness probe：仅检查进程存活
async fn liveness() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

/// GET /ready — K8s readiness probe：并行探测所有配置的上游可达性。
///
/// R10：
/// - 探测结果缓存 `health.cache_ttl`（默认 1s），避免探针风暴与上游抖动放大；
/// - 响应体裁剪为状态摘要（不再匿名返回上游 URL/错误串，防内部拓扑泄露）。
async fn readiness(State(state): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    const CACHE_KEY: &str = "bff:ready:result";
    let cfg = state.cfg();
    let hc = &cfg.health;
    let cache_ttl = hc.cache_ttl;

    // 0. 缓存命中
    if cache_ttl > std::time::Duration::ZERO {
        if let Some(bytes) = state.cache.get(CACHE_KEY).await {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                let code = v.get("_status").and_then(|s| s.as_u64()).unwrap_or(200) as u16;
                let body = v.get("body").cloned().unwrap_or(serde_json::Value::Null);
                return (
                    StatusCode::from_u16(code).unwrap_or(StatusCode::OK),
                    Json(body),
                );
            }
        }
    }

    // 确定上游列表：显式配置优先，否则从 routes 自动推导
    let upstreams: Vec<String> = if !hc.upstreams.is_empty() {
        hc.upstreams.clone()
    } else {
        let mut set = std::collections::HashSet::new();
        for route in &cfg.routes {
            if let crate::config::RouteType::Proxy = route.route_type {
                if let Some(ref u) = route.config.upstream {
                    set.insert(u.trim_end_matches('/').to_string());
                }
            }
        }
        let mut list: Vec<String> = set.into_iter().collect();
        list.sort();
        list
    };

    // 并行探测
    let probe_path = &hc.probe_path;
    let probe_timeout = hc.probe_timeout;

    let mut handles = Vec::with_capacity(upstreams.len());
    for upstream in &upstreams {
        let url = format!("{}{}", upstream.trim_end_matches('/'), probe_path);
        let client = state.http.clone();
        handles.push(tokio::spawn(async move {
            let result = tokio::time::timeout(probe_timeout, client.get(&url).send()).await;
            match result {
                Ok(Ok(resp)) => {
                    resp.status().is_success() || resp.status().as_u16() == 404 // 404 也算可达
                }
                _ => false,
            }
        }));
    }

    let total = upstreams.len();
    let mut unreachable = 0usize;
    for h in handles {
        if !matches!(h.await, Ok(true)) {
            unreachable += 1;
        }
    }

    let (status, summary) = if total == 0 {
        (StatusCode::OK, "no_upstreams_configured")
    } else if unreachable == 0 {
        (StatusCode::OK, "ready")
    } else if hc.allow_degraded {
        (StatusCode::OK, "degraded")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not_ready")
    };

    let body = serde_json::json!({
        "status": summary,
        "upstreams_total": total,
        "upstreams_unreachable": unreachable,
    });

    // 写缓存（含状态码）
    if cache_ttl > std::time::Duration::ZERO {
        let payload = serde_json::json!({
            "_status": status.as_u16(),
            "body": body,
        });
        if let Ok(bytes) = serde_json::to_vec(&payload) {
            state.cache.set(CACHE_KEY, bytes, cache_ttl).await;
        }
    }

    (status, Json(body))
}

/// GET /api/session — 返回当前会话状态（供前端 JS 读取，因为 cookie 是 HttpOnly）
async fn session_info(session: Session) -> Json<serde_json::Value> {
    let logged_in = session
        .get::<String>("oidc:current_provider")
        .await
        .ok()
        .flatten()
        .is_some();
    Json(serde_json::json!({
        "logged_in": logged_in,
    }))
}

/// GET/POST /pipeline/:name — 兼容旧入口，内部转为统一 Route 分发。
///
/// P0-5：该显式路由不经过统一路由分发器（`route_dispatcher::dispatch`），
/// 因此必须在此强制会话鉴权——否则任何匿名请求都可携带任意参数触发
/// pipeline 真实执行（含其访问内网上游的步骤）。
/// 需要匿名访问的 pipeline 应通过 `routes.yaml` 显式声明 `auth_required: false`，
/// 经统一分发器执行。
async fn run_pipeline(
    State(state): State<AppState>,
    session: Session,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // P0-5 鉴权：必须有有效会话（与统一分发器的 auth_required 语义对齐）
    crate::oidc::handlers::current_access_token(&session)
        .await
        .ok_or_else(|| AppError::unauthorized("未登录或会话已过期（/pipeline/:name 需要认证）"))?;

    let def = state
        .cfg()
        .pipelines
        .get(&name)
        .cloned()
        .ok_or_else(|| AppError::not_found(format!("pipeline 不存在: {}", name)))?;
    let start = std::time::Instant::now();
    let result = state.pipeline_executor.run(&name, &def, params).await;
    metrics::histogram!("bff_pipeline_duration_seconds", "pipeline" => name.clone())
        .record(start.elapsed().as_secs_f64());
    match result {
        Ok(r) => Ok((r.status, Json(r.body)).into_response()),
        Err(e) => Err(e),
    }
}

/// fallback：统一路由匹配 → 按 RouteType 分发；/api 前缀 404；其余走 SPA。
async fn fallback_handler(
    State(state): State<AppState>,
    session: Session,
    req: Request<Body>,
) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().to_string();

    // 1. 统一路由匹配（routes）—— clone route 以释放 cfg borrow
    let matched_route = {
        let cfg = state.cfg();
        route_dispatcher::match_route(&cfg.routes, &method, &path).cloned()
    };

    if let Some(route) = matched_route {
        return route_dispatcher::dispatch(&state, &route, &session, req)
            .await
            .unwrap_or_else(|e| e.into_response());
    }

    // 2. /api 与 /admin/api 前缀 → 404（F13：业务端口不存在管理面 API，
    // 原行为把 /admin/api/* 当 SPA 路由返回 200 + HTML，客户端无法区分路径错误与成功；
    // 其余 /admin/* 前端路由仍走 SPA fallback）
    if path.starts_with("/api/") || path.starts_with("/admin/api/") {
        return AppError::not_found("无匹配 API 路由").into_response();
    }

    // 3. SPA fallback
    serve_spa(&state, req).await
}

/// WebSocket 升级处理器：匹配路由 → 鉴权 → 建立双向隧道。
///
/// S6：
/// - 仅 `type: proxy` 且 `proxy_mode: websocket|auto` 的路由允许升级
///   （原实现任何路径前缀命中的路由都能建 WS 隧道）；
/// - 按 `auth_required` 强制会话鉴权；需要认证时向**上游握手**注入 Bearer。
async fn ws_upgrade_handler(
    State(state): State<AppState>,
    session: Session,
    ws: WebSocketUpgrade,
    req: Request<Body>,
) -> Response {
    let path = req.uri().path().to_string();

    let route = {
        let cfg = state.cfg();
        route_dispatcher::match_route(&cfg.routes, "GET", &path).cloned()
    };

    let route = match route {
        Some(r) => r,
        None => return AppError::not_found("无匹配 WebSocket 路由").into_response(),
    };

    if route.route_type != crate::config::RouteType::Proxy {
        return AppError::not_found("无匹配 WebSocket 路由").into_response();
    }
    if !matches!(route.config.proxy_mode.as_str(), "websocket" | "auto") {
        return AppError::bad_request("该路由未启用 WebSocket 代理模式").into_response();
    }

    let upstream = match route.config.upstream.as_deref() {
        Some(u) => u.trim_end_matches('/').to_string(),
        None => return AppError::bad_request("WebSocket 路由缺少 upstream").into_response(),
    };

    // S6：鉴权（与统一分发器同一语义）
    let auth_token = if route.auth_required {
        match oidc::current_access_token(&session).await {
            Some(t) => Some(t),
            None => {
                return AppError::unauthorized("未登录或会话已过期").into_response();
            }
        }
    } else {
        None
    };

    // 尊重 strip_prefix 配置（与 forward_request 保持一致）
    let suffix = if route.config.strip_prefix {
        path.strip_prefix(&route.path).unwrap_or("")
    } else {
        &path
    };
    let upstream_ws = upstream
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    let url = format!("{}{}", upstream_ws, suffix);

    tracing::info!(%path, %url, strip_prefix=route.config.strip_prefix, auth=route.auth_required, "WebSocket 升级请求");

    let tunnel_cfg = {
        let cfg = state.cfg();
        tunnel::TunnelConfig {
            connect_timeout: cfg.websocket.connect_timeout,
            idle_timeout: cfg.websocket.idle_timeout,
            heartbeat_interval: cfg.websocket.heartbeat_interval,
            max_message_bytes: cfg.websocket.max_message_bytes,
        }
    };

    ws.on_upgrade(move |client_ws| tunnel::ws_tunnel(client_ws, url, auth_token, tunnel_cfg))
}

/// SPA 静态资源 + 前端路由 fallback 到 index.html。
async fn serve_spa(state: &AppState, req: Request<Body>) -> Response {
    let dir = state.cfg().spa.dir.clone();
    let index = format!("{}/index.html", dir.trim_end_matches('/'));
    if !std::path::Path::new(&index).is_file() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "SPA 资源目录不存在", "dir": dir})),
        )
            .into_response();
    }
    let service = ServeDir::new(&dir).fallback(ServeFile::new(index));
    match service.oneshot(req).await {
        Ok(resp) => resp.into_response(),
        Err(_) => AppError::internal("静态资源服务异常").into_response(),
    }
}

/// 请求计数指标（O1：路径标签低基数化）。
///
/// 原实现直接用原始 URL path 作为标签：任何扫描器路径（`/.env`、`/wp-login.php` …）
/// 都会经 SPA fallback 返回并被计数 → 攻击者可用任意 URL 无界撑大标签基数。
/// 现在归一化为：命中路由模板（有序集合）→ 模板；已知固定路径 → 自身；
/// `/assets/*` → 常量；其余 → `other`。
async fn metrics_middleware(
    State(state): State<AppState>,
    req: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    let method = req.method().to_string();
    let path = metrics_path_label(&state, req.uri().path());
    let start = std::time::Instant::now();
    let resp = next.run(req).await;
    let status = resp.status().as_u16().to_string();
    // O2：全局请求延迟直方图（Prometheus 侧可算 P50/P95/P99）
    metrics::histogram!(
        "bff_http_request_duration_seconds",
        "method" => method.clone(),
        "path" => path.clone(),
    )
    .record(start.elapsed().as_secs_f64());
    metrics::counter!(
        "bff_http_requests_total",
        "method" => method,
        "path" => path,
        "status" => status,
    )
    .increment(1);
    resp
}

/// 将请求路径归一化为有限标签集（含路由模板与固定路径），防止基数爆炸。
fn metrics_path_label(state: &AppState, path: &str) -> String {
    // 1) 命中配置路由 → 用路由模板（路由数量由配置固定）
    if let Some(route) = route_dispatcher::match_route(&state.cfg().routes, "GET", path) {
        return route.path.clone();
    }
    // 2) 固定路径
    const FIXED: &[&str] = &[
        "/login",
        "/auth/callback",
        "/logout",
        "/live",
        "/ready",
        "/api/session",
    ];
    if FIXED.contains(&path) {
        return path.to_string();
    }
    if path.starts_with("/assets/") {
        return "/assets/*".to_string();
    }
    if path.starts_with("/pipeline/") {
        return "/pipeline/:name".to_string();
    }
    if path.starts_with("/ws") {
        return "/ws/*".to_string();
    }
    "other".to_string()
}
