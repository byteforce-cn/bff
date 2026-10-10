//! Host 白名单校验中间件（§6.3）：非豁免路径 Host 不命中白名单 → 421 Misdirected Request。
//!
//! 规则：
//! - `/live`、`/ready` 无条件豁免（探针来源是节点 IP、Host 是 Pod IP，且这两个
//!   端点不返回敏感信息、不参与 redirect_uri 推导）；
//! - legacy 且未启用 `server.enforce_host` → 行为冻结，原样放行；
//! - Host 归一化（大小写不敏感、剥离端口、IPv6 方括号归一化、去尾点）后必须命中
//!   站点 `allowed_hosts`（`server_names ∪ {public_base_url 主机}`）；
//! - 未配置站点 `public_base_url`（dev 语义）时 loopback 主机名作开发兜底放行；
//!   配置后（prod 语义）loopback 也拒绝；
//! - 其余（含 Host 缺失 / 不可解析）→ 421。
//!
//! 层序敏感：必须位于 session layer 之外——先于会话建立执行，伪造 Host
//! 不会为攻击者建立会话（§6.3 中间件顺序）。
use crate::site::SiteHandle;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Extension, State};
use axum::http::{header::HOST, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

/// 豁免 Host 校验的探针路径（§6.3）。
const EXEMPT_PATHS: [&str; 2] = ["/live", "/ready"];

/// 归一化请求 Host（§6.3）：剥离端口、IPv6 方括号归一化、小写、去尾点。
///
/// 非法（空白/控制字符、协议/路径、裸 IPv6 无方括号、空主机名）→ `None`。
/// 与配置侧 `config::normalize_host` 同一套归一化语义，保证比较一致。
fn normalize_request_host(host: &str) -> Option<String> {
    let host = host.trim();
    let bare = if let Some(rest) = host.strip_prefix('[') {
        // IPv6 字面量：[::1] 或 [::1]:port；方括号必须闭合，端口只能在右括号之后
        let (inner, after) = rest.split_once(']')?;
        if !after.is_empty() && !after.starts_with(':') {
            return None;
        }
        format!("[{inner}]")
    } else if let Some((name, _port)) = host.split_once(':') {
        // 主机名:端口；裸 IPv6 不含方括号 → 主机名部分为空 → normalize_host 拒绝
        name.to_string()
    } else {
        host.to_string()
    };
    crate::config::normalize_host(&bare)
}

/// loopback 主机名判定（`localhost` / `127.0.0.1` / `[::1]`，剥离端口与方括号）。
///
/// 自 `src/oidc/handlers.rs` 迁入（§6.3 复用）：OIDC `base_url_from` 与 Host
/// 校验中间件共用同一 dev 兜底语义。
pub(crate) fn is_loopback_host(host: &str) -> bool {
    let host = host.trim();
    let hostname = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or_default()
    };
    matches!(hostname, "localhost" | "127.0.0.1" | "::1")
}

/// §6.3：非豁免路径 Host 不命中（含缺失/不可解析）→ 421 Misdirected Request。
fn misdirected_response() -> Response {
    (
        StatusCode::MISDIRECTED_REQUEST,
        axum::Json(serde_json::json!({"error": "Misdirected Request"})),
    )
        .into_response()
}

/// Host 白名单中间件（§6.3）。
///
/// 层序：位于站点 `Extension<Arc<SiteHandle>>` 之内、session layer 之外
/// （`build_site_router` 挂载），先于会话建立执行。
pub async fn host_validation_middleware(
    State(state): State<AppState>,
    Extension(handle): Extension<Arc<SiteHandle>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    // 探针路径豁免容忍尾斜杠（`/live/` 同 `/live`）
    if EXEMPT_PATHS.contains(&req.uri().path().trim_end_matches('/')) {
        return next.run(req).await;
    }

    let view = state.site_view(&handle.name).expect("site view");
    // legacy 行为冻结：未显式开启 `server.enforce_host` 时全局 421 不启用
    if view.legacy && !state.cfg().server.enforce_host {
        return next.run(req).await;
    }

    let Some(raw_host) = req.headers().get(HOST).and_then(|h| h.to_str().ok()) else {
        tracing::warn!(site = %view.name, "Host 头缺失，拒绝请求（421）");
        return misdirected_response();
    };
    let Some(host) = normalize_request_host(raw_host) else {
        tracing::warn!(site = %view.name, host = %raw_host, "Host 不可解析，拒绝请求（421）");
        return misdirected_response();
    };

    let allowed = view.allowed_hosts.iter().any(|h| h == &host)
        || (view.public_base_url.is_none() && is_loopback_host(&host));
    if !allowed {
        tracing::warn!(site = %view.name, host = %raw_host, "Host 不在白名单，拒绝请求（421）");
        return misdirected_response();
    }
    next.run(req).await
}
