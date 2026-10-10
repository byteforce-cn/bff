//! 业务端口（8080）路由：OIDC、统一路由分发、SPA 发布、WebSocket 升级。
use crate::middleware::token_refresh::token_refresh_middleware;
use crate::oidc::handlers as oidc;
use crate::server::route_dispatcher;
use crate::server::tunnel;
use crate::site::{SiteCtx, SiteHandle, SiteView};
use crate::state::AppState;
use crate::utils::AppError;
use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use std::collections::HashMap;
use std::sync::Arc;
use tower::ServiceExt;
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tower_sessions::Session;

/// §6.1 多站点主路径：按站点句柄构建业务路由（每个 listener 一个 router）。
///
/// 路由与 handler 与 legacy 完全一致；站点差异全部由最外层注入的
/// `Extension<Arc<SiteHandle>>` 驱动：session 层取 `handle.session_layer`，
/// 安全响应头 / SPA 目录 / metrics site 标签 / 站点令牌鉴权按请求从 `SiteView`
/// 解析（配置热生效，§9/§10）。
pub fn build_site_router(state: AppState, handle: Arc<SiteHandle>) -> anyhow::Result<Router> {
    // §6.1：站点上下文（handler 闭包内用 Extension 重新取句柄构造 SiteCtx；
    // 视图必须每请求解析，此处仅用于启动期存在性校验与日志）
    let site = SiteCtx {
        handle: handle.as_ref(),
        view: state.site_view(&handle.name).expect("site view 必须存在"),
    };
    tracing::debug!(
        site = %site.view.name,
        port = site.view.port,
        "构建站点业务路由"
    );

    let cfg = state.cfg().clone();
    // §5.2：会话层启动时按 profile 预构建，站点 router 用句柄引用的层
    let session_layer = handle.session_layer.clone();

    // Trace ID: 为每个请求生成 UUID 并传播到响应头
    let request_id_layer = SetRequestIdLayer::new(
        axum::http::HeaderName::from_static("x-request-id"),
        MakeRequestUuid,
    );

    // CORS 默认收紧——仅显式 `permissive: true` 才全开；
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

    // 安全响应头由 `security_headers_middleware` 按请求站点应用（§9），
    // 不再在构建期克隆全局配置。

    // gzip 压缩（原依赖 tower-http "compression-gzip" 特性但从未挂载 CompressionLayer）。
    // 谓词排除 `text/event-stream`：SSE 需要逐块低延迟，不能被压缩缓冲。
    let compression_layer = CompressionLayer::new()
        .compress_when(DefaultPredicate::new().and(NotForContentType::new("text/event-stream")));

    let mut app = Router::new()
        .route("/login", get(oidc::login))
        .route("/logout", get(oidc::logout))
        .route("/live", get(liveness));

    // 按 provider 配置的 `callback_path` 动态注册回调路由（去重），
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
        .layer(session_layer)
        // W3C traceparent 注入/传播。
        // ⚠️ 层序敏感：必须位于 TraceLayer（外层，创建 http.request span）之内、
        // 且在任何会创建子 span 的层（如 tower-sessions 的 `call` span）之外——
        // 否则 Span::current() 不是请求 span，traceparent 的 span-id 会与导出 span 不一致。
        .layer(axum::middleware::from_fn(
            crate::middleware::trace_context::trace_context_middleware,
        ))
        .layer(request_id_layer)
        .layer(PropagateRequestIdLayer::new(
            axum::http::HeaderName::from_static("x-request-id"),
        ))
        // span 遵循 OTel HTTP semconv，入站 traceparent 作为 OTel 父上下文
        // （导出层未注册时 set_parent 无副作用，行为与默认 TraceLayer 一致）
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(crate::middleware::trace_context::BffMakeSpan)
                .on_response(crate::middleware::trace_context::RecordStatusOnResponse),
        )
        .layer(cors_layer)
        // gzip（SSE 已由谓词排除）
        .layer(compression_layer)
        // 全局限流（tower-governor）：与 CSP csp_overrides 同风格按路径前缀收窄，
        // SPA 静态资源等 skip_path_prefixes 命中的请求不消耗全局限流令牌，其余路径保持限流
        .layer(axum::middleware::from_fn_with_state(
            crate::middleware::rate_limit_skip::rate_limit_skip_state(
                cfg.rate_limit.per_second,
                cfg.rate_limit.burst_size,
                cfg.rate_limit.skip_path_prefixes.clone(),
                // 全局限流同样按真实客户端 IP 建桶（信任的代理跳数复用认证限流配置）
                cfg.auth_rate_limit.trusted_proxies,
            ),
            crate::middleware::rate_limit_skip::rate_limit_skip_middleware,
        ))
        // 认证端点 per-IP 限流（网络层纵深防御；未启用/未命中路径时原样放行）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::ip_rate_limit::ip_rate_limit_middleware,
        ))
        // 安全响应头（§9：按请求站点读预构建值，零解析成本）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            security_headers_middleware,
        ))
        // 请求体大小限制
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            cfg.body_limit.max_bytes,
        ))
        .with_state(state.clone());
    // Host 白名单（§6.3）：非豁免路径 Host 不命中 → 421。层序敏感：位于站点句柄
    // Extension 之内、session layer 之外——先于会话建立执行，伪造 Host 不建立会话。
    // 站点句柄 Extension 最后调用 `.layer` → 最外层。
    Ok(app
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::middleware::host_validation::host_validation_middleware,
        ))
        .layer(axum::Extension(handle)))
}

/// legacy 模式便利入口（§6.1）：`cfg.sites` 非空时拒绝——显式多站点配置必须
/// 逐站点调用 `build_site_router`；否则取 legacy `default` 句柄委托
/// （现有测试与嵌入方零改动）。
pub fn build_business_router(state: AppState) -> anyhow::Result<Router> {
    let cfg = state.cfg().clone();
    if !cfg.sites.is_empty() {
        anyhow::bail!("显式多站点配置请使用 build_site_router");
    }
    let handle = state
        .site_handles()?
        .into_iter()
        .find(|h| h.legacy)
        .ok_or_else(|| anyhow::anyhow!("缺少 legacy default 站点句柄"))?;
    build_site_router(state, handle)
}

/// GET /live — K8s liveness probe：仅检查进程存活
async fn liveness() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

/// GET /ready — K8s readiness probe：并行探测所有配置的上游可达性。
///
///
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

/// GET /api/session — 返回当前会话状态（供前端 JS 读取，因为 cookie 是 HttpOnly）。
/// 站点感知（§7.2）：logged_in = 站点当前 provider 有令牌；provider 为站点维度
/// current provider（未登录时为 null，兼容 legacy 响应形状）。
async fn session_info(
    State(state): State<AppState>,
    Extension(handle): Extension<Arc<SiteHandle>>,
    session: Session,
) -> Json<serde_json::Value> {
    let view = state.site_view(&handle.name).expect("site view");
    let provider = view.current_provider(&session).await;
    Json(serde_json::json!({
        "logged_in": provider.is_some(),
        "provider": provider,
    }))
}

/// GET/POST /pipeline/:name — 兼容旧入口，内部转为统一 Route 分发。
///
/// 该显式路由不经过统一路由分发器（`route_dispatcher::dispatch`），
/// 因此必须在此强制会话鉴权——否则任何匿名请求都可携带任意参数触发
/// pipeline 真实执行（含其访问内网上游的步骤）。
/// 需要匿名访问的 pipeline 应通过 `routes.yaml` 显式声明 `auth_required: false`，
/// 经统一分发器执行。
async fn run_pipeline(
    State(state): State<AppState>,
    Extension(handle): Extension<Arc<SiteHandle>>,
    session: Session,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 鉴权：必须有有效会话（与统一分发器的 auth_required 语义对齐）
    let view = state.site_view(&handle.name).expect("site view");
    crate::oidc::handlers::current_access_token(&view, &session)
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
    Extension(handle): Extension<Arc<SiteHandle>>,
    session: Session,
    req: Request<Body>,
) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().to_string();
    // §6.1：显式站点上下文（legacy 路由固定为 default 句柄）
    let ctx = SiteCtx {
        handle: &handle,
        view: state.site_view(&handle.name).expect("site view"),
    };

    // 1. 统一路由匹配（routes）—— clone route 以释放 cfg borrow
    let matched_route = {
        let cfg = state.cfg();
        route_dispatcher::match_route(&cfg.routes, &ctx.handle.name, &method, &path).cloned()
    };

    if let Some(route) = matched_route {
        return route_dispatcher::dispatch(&state, &ctx, &route, &session, req)
            .await
            .unwrap_or_else(|e| e.into_response());
    }

    // 2. /api 与 /admin/api 前缀 → 404（业务端口不存在管理面 API，
    // 原行为把 /admin/api/* 当 SPA 路由返回 200 + HTML，客户端无法区分路径错误与成功；
    // 其余 /admin/* 前端路由仍走 SPA fallback）
    if path.starts_with("/api/") || path.starts_with("/admin/api/") {
        return AppError::not_found("无匹配 API 路由").into_response();
    }

    // 3. SPA fallback（使用站点 spa.dir，§9）
    serve_spa(&ctx.view, req).await
}

/// WebSocket 升级处理器：匹配路由 → 鉴权 → 建立双向隧道。
///
///
/// - 仅 `type: proxy` 且 `proxy_mode: websocket|auto` 的路由允许升级
///   （原实现任何路径前缀命中的路由都能建 WS 隧道）；
/// - 按 `auth_required` 强制会话鉴权；需要认证时向**上游握手**注入 Bearer。
async fn ws_upgrade_handler(
    State(state): State<AppState>,
    Extension(handle): Extension<Arc<SiteHandle>>,
    session: Session,
    ws: WebSocketUpgrade,
    req: Request<Body>,
) -> Response {
    let path = req.uri().path().to_string();
    // §6.1：显式站点上下文（legacy 路由固定为 default 句柄）
    let ctx = SiteCtx {
        handle: &handle,
        view: state.site_view(&handle.name).expect("site view"),
    };

    let route = {
        let cfg = state.cfg();
        route_dispatcher::match_route(&cfg.routes, &ctx.handle.name, "GET", &path).cloned()
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

    // 鉴权（与统一分发器同一语义，站点维度）
    let auth_token = if route.auth_required {
        match oidc::current_access_token(&ctx.view, &session).await {
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

/// SPA 静态资源 + 前端路由 fallback 到 index.html（使用站点 spa.dir，§9）。
async fn serve_spa(view: &SiteView, req: Request<Body>) -> Response {
    let dir = view.spa_dir.clone();
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

/// 安全响应头中间件（§9）：按请求站点从 `SiteView` 读预构建值并应用——
/// 请求路径零字符串解析 / 零 `HeaderValue` 构造；站点覆盖（含 CSP 整体替换）
/// 随配置热生效。
async fn security_headers_middleware(
    State(state): State<AppState>,
    Extension(handle): Extension<Arc<SiteHandle>>,
    req: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path().to_string();
    let view = state.site_view(&handle.name).expect("site view");
    let mut resp = next.run(req).await;
    view.security_headers.apply(&path, resp.headers_mut());
    resp
}

/// 请求计数指标（路径标签低基数化，site 标签由站点句柄决定，§10）。
///
/// 原实现直接用原始 URL path 作为标签：任何扫描器路径（`/.env`、`/wp-login.php` …）
/// 都会经 SPA fallback 返回并被计数 → 攻击者可用任意 URL 无界撑大标签基数。
/// 现在归一化为：命中路由模板（有序集合）→ 模板；已知固定路径 → 自身；
/// `/assets/*` → 常量；其余 → `other`。
async fn metrics_middleware(
    State(state): State<AppState>,
    Extension(handle): Extension<Arc<SiteHandle>>,
    req: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    let method = req.method().to_string();
    let raw_path = req.uri().path().to_string();
    let path = metrics_path_label(&state, &handle.name, &raw_path);
    let start = std::time::Instant::now();
    let resp = next.run(req).await;
    // §10：K8s 探针不进入业务请求指标（会污染低频站点的 P95/P99）
    if is_probe_path(&raw_path) {
        return resp;
    }
    let status = resp.status().as_u16().to_string();
    // 全局请求延迟直方图（Prometheus 侧可算 P50/P95/P99）
    metrics::histogram!(
        "bff_http_request_duration_seconds",
        "method" => method.clone(),
        "path" => path.clone(),
        "site" => handle.name.clone(),
    )
    .record(start.elapsed().as_secs_f64());
    metrics::counter!(
        "bff_http_requests_total",
        "method" => method,
        "path" => path,
        "status" => status,
        "site" => handle.name.clone(),
    )
    .increment(1);
    resp
}

/// 探针路径判定（§10）：`/live`、`/ready` 不计入业务请求指标。
pub(crate) fn is_probe_path(path: &str) -> bool {
    path == "/live" || path == "/ready"
}

/// 将请求路径归一化为有限标签集（含路由模板与固定路径），防止基数爆炸。
fn metrics_path_label(state: &AppState, site: &str, path: &str) -> String {
    // 1) 命中配置路由 → 用路由模板（路由数量由配置固定；站点过滤 §5.3）
    if let Some(route) = route_dispatcher::match_route(&state.cfg().routes, site, "GET", path) {
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

#[cfg(test)]
mod tests {
    use super::is_probe_path;

    /// §10：探针路径不计入业务请求指标（端到端断言见
    /// `tests/test_multi_site_runtime.rs::metrics_carry_site_label_and_probes_are_excluded`）。
    #[test]
    fn probe_paths_are_excluded_from_business_metrics() {
        assert!(is_probe_path("/live"));
        assert!(is_probe_path("/ready"));
        assert!(!is_probe_path("/api/x"));
        // 前缀相同但不等于探针路径的请求仍进入业务指标
        assert!(!is_probe_path("/livez"));
        assert!(!is_probe_path("/ready.html"));
    }
}
