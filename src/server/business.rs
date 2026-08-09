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
use std::sync::Arc;
use tower::ServiceExt;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::GovernorLayer;
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
        MakeRequestUuid::default(),
    );
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(cfg.rate_limit.per_second)
            .burst_size(cfg.rate_limit.burst_size)
            .finish()
            .expect("限流配置非法"),
    );

    // CORS：根据配置选择 permissive 或白名单模式
    let cors_layer = if cfg.cors.permissive || cfg.cors.allowed_origins.is_empty() {
        CorsLayer::permissive()
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

    let app = Router::new()
        .route("/login", get(oidc::login))
        .route("/auth/callback", get(oidc::callback))
        .route("/logout", get(oidc::logout))
        .route("/live", get(liveness))
        .route("/ready", get(readiness))
        .route("/api/session", get(session_info))
        // 兼容旧 /pipeline/:name 路由（内部转为统一 Route 分发）
        .route("/pipeline/:name", get(run_pipeline).post(run_pipeline))
        // WebSocket 升级专用路由（在 fallback 之前匹配）
        .route("/ws", get(ws_upgrade_handler))
        .route("/ws/*rest", get(ws_upgrade_handler))
        .fallback(fallback_handler)
        .layer(axum::middleware::from_fn(metrics_middleware))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            token_refresh_middleware,
        ))
        .layer(session_layer)
        .layer(request_id_layer)
        .layer(PropagateRequestIdLayer::new(
            axum::http::HeaderName::from_static("x-request-id"),
        ))
        .layer(TraceLayer::new_for_http())
        .layer(cors_layer)
        .layer(GovernorLayer {
            config: governor_conf,
        })
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
/// 上游列表优先取自 `health.upstreams`；若为空则从 routes 中自动提取 proxy 类 upstream 去重。
async fn readiness(State(state): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    let cfg = state.cfg();
    let hc = &cfg.health;

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
    let mut results = serde_json::Map::new();

    let mut handles = Vec::with_capacity(upstreams.len());
    for upstream in &upstreams {
        let url = format!("{}{}", upstream.trim_end_matches('/'), probe_path);
        let client = state.http.clone();
        let u = upstream.clone();
        handles.push(tokio::spawn(async move {
            let start = std::time::Instant::now();
            let result = tokio::time::timeout(probe_timeout, client.get(&url).send()).await;
            let latency_ms = start.elapsed().as_millis() as u64;
            match result {
                Ok(Ok(resp)) => {
                    let reachable = resp.status().is_success() || resp.status().as_u16() == 404; // 404 也算可达
                    (u, reachable, latency_ms, None::<String>)
                }
                Ok(Err(e)) => (u, false, latency_ms, Some(format!("{}", e))),
                Err(_) => (u, false, latency_ms, Some("timeout".to_string())),
            }
        }));
    }

    let mut all_reachable = true;
    for h in handles {
        if let Ok((name, reachable, latency_ms, error)) = h.await {
            let mut entry = serde_json::Map::new();
            entry.insert("reachable".into(), serde_json::Value::Bool(reachable));
            entry.insert(
                "latency_ms".into(),
                serde_json::Value::Number(serde_json::Number::from(latency_ms)),
            );
            if let Some(err) = error {
                entry.insert("error".into(), serde_json::Value::String(err));
            }
            results.insert(name, serde_json::Value::Object(entry));
            if !reachable {
                all_reachable = false;
            }
        }
    }

    let (status, summary) = if upstreams.is_empty() {
        (StatusCode::OK, "no_upstreams_configured")
    } else if all_reachable {
        (StatusCode::OK, "ready")
    } else if hc.allow_degraded {
        (StatusCode::OK, "degraded")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not_ready")
    };

    let body = serde_json::json!({
        "status": summary,
        "upstreams": results,
    });

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

/// GET/POST /pipeline/:name — 兼容旧入口，内部转为统一 Route 分发
async fn run_pipeline(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
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

    // 2. /api 前缀 → 404
    if path.starts_with("/api/") {
        return AppError::not_found("无匹配 API 路由").into_response();
    }

    // 3. SPA fallback
    serve_spa(&state, req).await
}

/// WebSocket 升级处理器：匹配路由 → 建立双向隧道。
async fn ws_upgrade_handler(
    State(state): State<AppState>,
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

    let upstream = match route.config.upstream.as_deref() {
        Some(u) => u.trim_end_matches('/').to_string(),
        None => return AppError::bad_request("WebSocket 路由缺少 upstream").into_response(),
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

    tracing::info!(%path, %url, strip_prefix=route.config.strip_prefix, "WebSocket 升级请求");

    ws.on_upgrade(move |client_ws| tunnel::ws_tunnel(client_ws, url, None))
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

/// 请求计数指标。
async fn metrics_middleware(req: Request<Body>, next: axum::middleware::Next) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let resp = next.run(req).await;
    metrics::counter!(
        "bff_http_requests_total",
        "method" => method,
        "path" => path,
        "status" => resp.status().as_u16().to_string(),
    )
    .increment(1);
    resp
}
