//! 反向代理：按 routes.yaml 的 path 前缀转发到 upstream，注入 Bearer 令牌，带熔断。
//!
//! 支持三种代理模式：
//! - "http": 一次性请求-响应（默认，现有行为）
//! - "sse": 流式透传 SSE（逐 chunk relay）
//! - "websocket": 交由 business.rs 的 WS upgrade handler 处理
//! - "auto": 根据请求头自动检测（Upgrade: websocket → WS，否则 → HTTP）
//!
//! 令牌刷新：上游返回 401 时自动尝试刷新 access token 并重试一次。

use crate::config::RouteDef;
use crate::oidc::handlers::{current_access_token, current_tokens, force_refresh};
use crate::server::sse_proxy;
use crate::state::AppState;
use crate::utils::AppError;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::Response;
use tower_sessions::Session;

/// 获取 Bearer token（若路由需要认证）。
///
/// - 未配置 `token_exchange`：现状（直接注入会话 access token）；
/// - 配置 `token_exchange`：经 RFC 8693 交换面向上游资源的 token（含缓存与失败语义）。
async fn resolve_auth_token(
    state: &AppState,
    session: &Session,
    route: &RouteDef,
) -> Result<Option<String>, AppError> {
    if !route.auth_required {
        return Ok(None);
    }
    if let Some(te) = &route.config.token_exchange {
        let result = crate::server::token_exchange::resolve(state, session, te).await?;
        Ok(Some(result.access_token))
    } else {
        let token = current_access_token(session)
            .await
            .ok_or_else(|| AppError::unauthorized("未登录或会话已过期"))?;
        Ok(Some(token))
    }
}

/// 使用 RouteDef 的转发（统一路由 v2）—— 按 proxy_mode 分发。
pub async fn forward_request(
    state: &AppState,
    session: &Session,
    route: &RouteDef,
    upstream: &str,
    req: Request<Body>,
) -> Result<Response, AppError> {
    let upstream = upstream.trim_end_matches('/');

    // 熔断检查
    if !state.breakers.allow(upstream).await {
        metrics::counter!("bff_proxy_rejected_total", "upstream" => upstream.to_string())
            .increment(1);
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("上游熔断中: {}", upstream),
        ));
    }

    let path = req.uri().path();
    let suffix = if route.config.strip_prefix {
        path.strip_prefix(&route.path).unwrap_or("")
    } else {
        path
    };
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();
    let url = format!("{}{}{}", upstream, suffix, query);

    let method = req.method().clone();
    // 提取请求 ID 用于传播到上游
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    // 透传原始请求头（实体头/媒体类型/自定义头等），跳过硬编码与 hop-by-hop 头。
    // 关键：必须恢复 Content-Type 等实体头——否则重建 reqwest 请求时，
    // Vec<u8> body 会被 reqwest 默认成 application/octet-stream（本 issue 根因）。
    let passthrough_headers = passthrough_headers(req.headers());
    let body_bytes = axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024)
        .await
        .map_err(|e| AppError::bad_request(format!("读取请求体失败: {}", e)))?;

    let auth_token = resolve_auth_token(state, session, route).await?;

    let proxy_mode = route.config.proxy_mode.as_str();

    match proxy_mode {
        "sse" => {
            let result = sse_proxy::sse_stream(
                &state.http,
                &url,
                reqwest::Method::from_bytes(method.as_str().as_bytes())
                    .map_err(|e| AppError::bad_request(format!("非法方法: {}", e)))?,
                body_bytes.to_vec(),
                auth_token,
                request_id.as_deref(),
                &passthrough_headers,
            )
            .await;

            match &result {
                Ok(_) => state.breakers.record_success(upstream).await,
                Err(_) => {
                    state.breakers.record_failure(upstream).await;
                    metrics::counter!("bff_proxy_error_total", "upstream" => upstream.to_string())
                        .increment(1);
                }
            }
            result
        }
        // "http" | "auto" | "" | 其他 → 标准一次性 HTTP 代理（含 401 刷新重试）
        _ => {
            let method_c = method.clone();
            let body_c = body_bytes.to_vec();
            let auth_c = auth_token.clone();

            let resp = proxy_http(
                state,
                upstream,
                &url,
                method_c,
                body_c,
                auth_c,
                request_id.as_deref(),
                &passthrough_headers,
            )
            .await?;

            // 上游返回 401 且请求携带了 token → 刷新会话 token 后重试一次。
            // 必须复用 resolve_auth_token：exchange 路由会因 subject token 指纹变化
            // 自动重交换（§5.3/§6.3），普通路由则重新注入新的会话 access token。
            if resp.status() == StatusCode::UNAUTHORIZED && auth_token.is_some() {
                if let Some(tokens) = current_tokens(session).await {
                    if force_refresh(state, session, &tokens).await.is_ok() {
                        if let Ok(Some(new_token)) = resolve_auth_token(state, session, route).await
                        {
                            let retry_resp = proxy_http(
                                state,
                                upstream,
                                &url,
                                method,
                                body_bytes.to_vec(),
                                Some(new_token),
                                request_id.as_deref(),
                                &passthrough_headers,
                            )
                            .await?;
                            if retry_resp.status() != StatusCode::UNAUTHORIZED {
                                return Ok(retry_resp);
                            }
                        }
                    }
                }
            }
            Ok(resp)
        }
    }
}

/// 提取需要透传到 upstream 的原始请求头。
///
/// 跳过（避免与 BFF 语义冲突）：
/// - hop-by-hop 头：connection / keep-alive / proxy-connection / te / trailer /
///   transfer-encoding / upgrade
/// - 由 BFF 重新注入或基于 URL/body 推导的头：host（reqwest 按 URL 设置）、
///   authorization（BFF 统一注入会话 Bearer token）、cookie（会话 cookie 不外泄）、
///   content-length（reqwest 按 body 字节数重算）
///
/// 保留：content-type、content-encoding、accept、accept-encoding、accept-language、
/// 自定义 x-* 头等（透传优先）。
fn passthrough_headers(headers: &HeaderMap) -> HeaderMap {
    const SKIP: &[&str] = &[
        "host",
        "authorization",
        "cookie",
        "content-length",
        "connection",
        "keep-alive",
        "proxy-connection",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ];
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        if SKIP.contains(&name.as_str()) {
            continue;
        }
        out.insert(name.clone(), value.clone());
    }
    out
}

/// 标准 HTTP 代理：一次性 reqwest 请求-响应。
async fn proxy_http(
    state: &AppState,
    upstream: &str,
    url: &str,
    method: axum::http::Method,
    body: Vec<u8>,
    auth_token: Option<String>,
    request_id: Option<&str>,
    extra_headers: &HeaderMap,
) -> Result<Response, AppError> {
    let cfg = state.cfg();
    let max_retries = cfg.http_client.retry_max_attempts;
    let backoff = cfg.http_client.retry_backoff;
    let is_idempotent = method == axum::http::Method::GET || method == axum::http::Method::HEAD;

    let mut last_err = None;

    for attempt in 0..=max_retries {
        if attempt > 0 && is_idempotent {
            let delay = backoff * 2u32.pow(attempt - 1);
            tracing::debug!(attempt, ?delay, upstream, "代理重试");
            tokio::time::sleep(delay).await;
        }

        let mut out_req = state.http.request(
            reqwest::Method::from_bytes(method.as_str().as_bytes())
                .map_err(|e| AppError::bad_request(format!("非法方法: {}", e)))?,
            url,
        );

        if let Some(token) = &auth_token {
            out_req = out_req.bearer_auth(token.clone());
        }
        if let Some(rid) = request_id {
            out_req = out_req.header("x-request-id", rid);
        }
        if !body.is_empty() {
            out_req = out_req.body(body.clone());
        }
        // 在设置 body 之后恢复原始实体头：覆盖 reqwest 对 Vec<u8> 的默认
        // Content-Type: application/octet-stream（透传优先，本 issue 根因修复）。
        // 注意：axum 用 http 1.x、reqwest 经 openidconnect 用 http 0.2，需按字节转换类型。
        for (name, value) in extra_headers {
            if let (Ok(n), Ok(v)) = (
                reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
                reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
            ) {
                out_req = out_req.header(n, v);
            }
        }

        let result = out_req.send().await;
        match result {
            Ok(resp) => {
                let status = resp.status();
                if status.is_server_error() {
                    state.breakers.record_failure(upstream).await;
                    // 仅对幂等请求在服务端错误时重试
                    if is_idempotent && attempt < max_retries {
                        last_err = Some(AppError::bad_gateway(format!(
                            "上游 {} 返回 {}（将重试）",
                            upstream,
                            status.as_u16()
                        )));
                        continue;
                    }
                } else {
                    state.breakers.record_success(upstream).await;
                }
                let mut builder = Response::builder().status(status.as_u16());
                for (k, v) in resp.headers() {
                    if matches!(
                        k.as_str(),
                        "connection" | "transfer-encoding" | "keep-alive" | "upgrade"
                    ) {
                        continue;
                    }
                    if let (Ok(name), Ok(val)) = (
                        k.as_str().parse::<axum::http::HeaderName>(),
                        axum::http::HeaderValue::from_bytes(v.as_bytes()),
                    ) {
                        builder = builder.header(name, val);
                    }
                }
                let bytes = resp
                    .bytes()
                    .await
                    .map_err(|e| AppError::bad_gateway(e.to_string()))?;
                return builder
                    .body(Body::from(bytes.to_vec()))
                    .map_err(|e| AppError::internal(e.to_string()));
            }
            Err(e) => {
                state.breakers.record_failure(upstream).await;
                if is_idempotent && attempt < max_retries {
                    last_err = Some(AppError::bad_gateway(format!(
                        "上游调用失败: {}（将重试）",
                        e
                    )));
                    continue;
                }
                last_err = Some(AppError::bad_gateway(format!("上游调用失败: {}", e)));
            }
        }
    }

    // 所有重试均已耗尽
    metrics::counter!("bff_proxy_error_total", "upstream" => upstream.to_string()).increment(1);
    Err(last_err.unwrap_or_else(|| AppError::bad_gateway("未知代理错误")))
}
