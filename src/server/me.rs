//! `GET /api/me` — 当前用户与会话视图（同源只读，绝不含任何令牌）。
//!
//! 身份取会话中已存的 ID Token claims（本地解码，零网络）；权限视图
//! （`me.permissions`，缺省关闭）由 `resolve_permissions` 追加。
use crate::config::ResolvedMePermissions;
use crate::oidc::tokens::StoredTokens;
use crate::site::{SiteHandle, SiteView};
use crate::state::AppState;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use std::sync::Arc;
use tower_sessions::Session;

/// 本地解码 ID Token 载荷（不验签：令牌来自本端加密会话，非外部输入）。
/// 非法结构返回 `None`，由调用方回退到会话中的 `sub`。
pub(crate) fn decode_id_token_claims(id_token: &str) -> Option<serde_json::Value> {
    use base64::Engine;
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 内置身份字段：始终返回，且只接受字符串（`sub` 以会话为准）。
const BUILTIN_IDENTITY_FIELDS: [&str; 3] = ["preferred_username", "name", "email"];

/// 白名单身份字段：`sub`（以会话为准）+ 三个标准字符串 claim。
/// 其它 claims（groups / nonce / iat …）一律不透传。
#[cfg(test)]
pub(crate) fn identity_fields(sub: &str, claims: Option<&serde_json::Value>) -> serde_json::Value {
    identity_fields_with("test", sub, claims, &[])
}

/// 身份视图：内置字段 + 配置的 `me.identity_claims` 白名单（issue #129）。
///
/// 规则：
/// - `sub` 永远取自会话（ID Token 里的 `sub` 不可篡改身份）；
/// - 白名单 claim 只接受字符串/数字/布尔/字符串数组，其它类型整体丢弃；
/// - 敏感 claim（`nonce`/`at_hash`/`sid` …）即使配置了也始终拒绝；
/// - 响应体累计超过 [`crate::config::ME_IDENTITY_MAX_BYTES`] 后停止追加额外 claim
///   （内置字段优先保留，身份视图始终可用）。
///
/// 丢弃原因通过 `bff_me_identity_claim_dropped_total{reason}` 观测。
pub(crate) fn identity_fields_with(
    site: &str,
    sub: &str,
    claims: Option<&serde_json::Value>,
    allow: &[String],
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert("sub".into(), serde_json::Value::String(sub.to_string()));
    for key in BUILTIN_IDENTITY_FIELDS {
        if let Some(v) = claims.and_then(|c| c.get(key)).and_then(|v| v.as_str()) {
            out.insert(key.into(), serde_json::Value::String(v.to_string()));
        }
    }

    let mut size = serde_json::to_vec(&out).map_or(0, |b| b.len());
    for key in allow {
        // 启动期校验 + 解析阶段过滤已排除敏感项；这里兜底防热重载绕过。
        if crate::config::SENSITIVE_IDENTITY_CLAIMS.contains(&key.as_str()) {
            count_dropped("sensitive");
            continue;
        }
        let Some(value) = claims.and_then(|c| c.get(key)) else {
            continue; // 不存在：正常情况，不算降级
        };
        let Some(value) = normalize_claim_value(value) else {
            count_dropped("type");
            tracing::debug!(site, claim = %key, "身份视图：claim 类型不在白名单，未透传");
            continue;
        };
        let encoded_len = serde_json::to_vec(&value).map_or(usize::MAX, |b| b.len());
        // `,"key":` 的开销（引号 + 冒号 + 逗号）近似为 key 长度 + 6。
        if size + encoded_len + key.len() + 6 > crate::config::ME_IDENTITY_MAX_BYTES {
            count_dropped("size");
            tracing::debug!(site, claim = %key, "身份视图：响应体超过上限，claim 未透传");
            continue;
        }
        size += encoded_len + key.len() + 6;
        out.insert(key.clone(), value);
    }
    serde_json::Value::Object(out)
}

/// claim 允许的结构：字符串 / 数字 / 布尔 / 字符串数组。
fn normalize_claim_value(v: &serde_json::Value) -> Option<serde_json::Value> {
    match v {
        serde_json::Value::String(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::Bool(_) => Some(v.clone()),
        serde_json::Value::Array(items)
            if items.iter().all(|x| x.as_str().is_some()) && !items.is_empty() =>
        {
            Some(v.clone())
        }
        _ => None,
    }
}

fn count_dropped(reason: &'static str) {
    metrics::counter!("bff_me_identity_claim_dropped_total", "reason" => reason).increment(1);
}

/// GET /api/me — 站点感知：登录态与身份按站点当前 provider 解析（与 `/api/session` 同源）。
pub async fn me_handler(
    State(state): State<AppState>,
    Extension(handle): Extension<Arc<SiteHandle>>,
    session: Session,
) -> Response {
    let view = state.site_view(&handle.name).expect("site view");
    let Some(tokens) = view.current_tokens(&session).await else {
        return Json(logged_out()).into_response();
    };
    let id_token = tokens.id_token().ok().flatten();
    let claims = id_token.as_deref().and_then(decode_id_token_claims);
    let mut body = identity_fields_with(
        &view.name,
        &tokens.sub,
        claims.as_ref(),
        &view.me_identity_claims,
    );
    if let Some(obj) = body.as_object_mut() {
        obj.insert("logged_in".into(), serde_json::Value::Bool(true));
        obj.insert(
            "provider".into(),
            serde_json::Value::String(tokens.provider.clone()),
        );
        obj.insert(
            "expires_at".into(),
            serde_json::Value::from(tokens.expires_at),
        );
    }
    if let Some(cfg) = view.me_permissions.as_ref() {
        match resolve_permissions(&state, &view, &tokens, cfg).await {
            Ok(permissions) => {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("permissions".into(), permissions);
                }
            }
            Err(failure) => {
                // 默认降级：身份照常 200，只是不返回 permissions。
                // `required: true` 时把失败变成明确错误，便于后端区分「无权限」与「授权服务不可用」。
                if cfg.required {
                    let (status, code) = match failure {
                        ResolveFailure::Unauthorized => {
                            (StatusCode::UNAUTHORIZED, "permissions_unauthorized")
                        }
                        _ => (StatusCode::SERVICE_UNAVAILABLE, "permissions_unavailable"),
                    };
                    return (status, Json(serde_json::json!({"error": code}))).into_response();
                }
            }
        }
    }
    Json(body).into_response()
}

/// 权限视图解析结果校验（默认路径 `roles` / `permissions`）。
///
/// 两个路径至少命中一个且必须是字符串数组；结构非法返回 `None`（整体拒绝，不输出半截字段）。
#[cfg(test)]
pub(crate) fn permissions_fields(resolve_body: &serde_json::Value) -> Option<serde_json::Value> {
    map_resolve_response(
        resolve_body,
        &crate::config::MeResolveResponseConfig::default(),
    )
}

/// 按配置的响应映射把授权权威响应转成 `/api/me` 的 `permissions` 视图。
///
/// - `roles_path` / `permissions_path`：点分 JSON 路径（支持数字下标）；
/// - 至少一个路径命中，否则整体判为结构非法；
/// - `passthrough` 里的顶层字段原样附加（如 `sod_warnings`）。
pub(crate) fn map_resolve_response(
    resolve_body: &serde_json::Value,
    cfg: &crate::config::MeResolveResponseConfig,
) -> Option<serde_json::Value> {
    fn strings(v: &serde_json::Value) -> Option<Vec<String>> {
        let arr = v.as_array()?;
        Some(
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
        )
    }
    let roles_raw = json_path(resolve_body, &cfg.roles_path);
    let permissions_raw = json_path(resolve_body, &cfg.permissions_path);
    if roles_raw.is_none() && permissions_raw.is_none() {
        return None;
    }
    let roles = match roles_raw {
        Some(v) => strings(v)?,
        None => Vec::new(),
    };
    let permissions = match permissions_raw {
        Some(v) => strings(v)?,
        None => Vec::new(),
    };
    let mut out = serde_json::Map::new();
    out.insert("roles".into(), serde_json::json!(roles));
    out.insert("permissions".into(), serde_json::json!(permissions));
    for key in &cfg.passthrough {
        if let Some(v) = resolve_body.get(key) {
            out.insert(key.clone(), v.clone());
        }
    }
    Some(serde_json::Value::Object(out))
}

/// 点分 JSON 路径：`data.roles` / `items.0.name`；空段或非容器直接判为不存在。
fn json_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cur = value;
    for seg in path.split('.') {
        if seg.is_empty() {
            return None;
        }
        cur = match cur {
            serde_json::Value::Object(map) => map.get(seg)?,
            serde_json::Value::Array(items) => items.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// resolve 失败分类（决定降级/必需模式下的状态码与指标标签）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolveFailure {
    /// 授权权威返回 401/403：调用方身份不被接受。
    Unauthorized,
    /// 超时。
    Timeout,
    /// 其它非 2xx。
    UpstreamStatus,
    /// 响应体超限 / 非 JSON / 结构非法。
    InvalidResponse,
    /// 会话令牌不可用、模板渲染失败、连接失败等。
    Unavailable,
}

/// 模板渲染上下文（`now` 每次调用现取，保证 `as_of` 是请求时刻）。
struct ResolveCtx {
    site: String,
    sub: String,
    provider: String,
    access_token: String,
    now: String,
}

impl ResolveCtx {
    fn value(&self, name: &str) -> Option<&str> {
        match name {
            "sub" => Some(&self.sub),
            "provider" => Some(&self.provider),
            "site" => Some(&self.site),
            "access_token" => Some(&self.access_token),
            "now" => Some(&self.now),
            _ => None,
        }
    }
}

/// 渲染 `{{var}}` 模板（header 值：原样替换）。
fn render_template(tpl: &str, ctx: &ResolveCtx) -> Option<String> {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}")?;
        let name = after[..end].trim();
        out.push_str(ctx.value(name)?);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Some(out)
}

/// 渲染 body 模板：变量按 JSON 字符串转义注入，避免 claim 内容破坏 JSON 结构。
fn render_body_template(tpl: &str, ctx: &ResolveCtx) -> Option<String> {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}")?;
        let name = after[..end].trim();
        // 令牌只能进 Authorization 头：模板校验已在启动期拒绝，运行时再兜底。
        if !crate::config::ME_RESOLVE_BODY_VARS.contains(&name) {
            return None;
        }
        let raw = ctx.value(name)?;
        let quoted = serde_json::to_string(raw).ok()?;
        out.push_str(quoted.trim_matches('"'));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Some(out)
}

fn now_iso() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::new())
}

fn resolve_metric(site: &str, result: &'static str) {
    metrics::counter!(
        "bff_me_permissions_resolve_total",
        "site" => site.to_string(),
        "result" => result
    )
    .increment(1);
}

/// 调用下游授权权威解析权限（请求/响应映射见 `me.permissions.request/response`）。
///
/// 失败分类见 [`ResolveFailure`]；调用方决定降级还是 `required` 报错。
/// resolve 响应体上限：权限清单应远小于此；超出按失败处理（防下游异常时内存膨胀）。
const RESOLVE_MAX_BODY_BYTES: usize = 64 * 1024;

/// 成功结果按 `site + provider + sub` 缓存，TTL 见 `me.permissions.cache_ttl`。
async fn resolve_permissions(
    state: &AppState,
    site: &SiteView,
    tokens: &StoredTokens,
    cfg: &ResolvedMePermissions,
) -> Result<serde_json::Value, ResolveFailure> {
    // 长度前缀消除分隔符歧义（provider 只校验非空、sub 由 IdP 控制，两者都可能含 `:`）。
    let key = permissions_cache_key(&site.name, &tokens.provider, &tokens.sub);
    if let Some(bytes) = state.cache.get(&key).await {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            resolve_metric(&site.name, "cache_hit");
            return Ok(v);
        }
    }
    let access_token = match tokens.access_token() {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                site = %site.name,
                provider = %tokens.provider,
                sub = %tokens.sub,
                error = %e,
                "权限视图：access token 解密失败"
            );
            resolve_metric(&site.name, "unavailable");
            return Err(ResolveFailure::Unavailable);
        }
    };

    let ctx = ResolveCtx {
        site: site.name.clone(),
        sub: tokens.sub.clone(),
        provider: tokens.provider.clone(),
        access_token,
        now: now_iso(),
    };
    let method = reqwest::Method::from_bytes(cfg.request.method.to_uppercase().as_bytes())
        .map_err(|_| ResolveFailure::Unavailable)?;
    let mut rb = state
        .http
        .request(method.clone(), &cfg.resolve_url)
        .timeout(cfg.timeout);
    let mut has_content_type = false;
    let mut has_authorization = false;
    for (name, value) in &cfg.request.headers {
        let Some(rendered) = render_template(value, &ctx) else {
            resolve_metric(&site.name, "unavailable");
            return Err(ResolveFailure::Unavailable);
        };
        if name.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        if name.eq_ignore_ascii_case("authorization") {
            has_authorization = true;
        }
        rb = rb.header(name.as_str(), rendered);
    }
    if !has_authorization {
        rb = rb.header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", ctx.access_token),
        );
    }
    match &cfg.request.body {
        Some(tpl) => {
            let Some(rendered) = render_body_template(tpl, &ctx) else {
                resolve_metric(&site.name, "unavailable");
                return Err(ResolveFailure::Unavailable);
            };
            if !has_content_type {
                rb = rb.header(reqwest::header::CONTENT_TYPE, "application/json");
            }
            rb = rb.body(rendered);
        }
        None if method != reqwest::Method::GET => {
            // 缺省请求体与 #94 完全一致（不配置 = 旧契约）。
            if !has_content_type {
                rb = rb.header(reqwest::header::CONTENT_TYPE, "application/json");
            }
            rb = rb.json(&serde_json::json!({
                "sub": ctx.sub,
                "provider": ctx.provider,
            }));
        }
        None => {}
    }

    // `RequestBuilder::timeout` 覆盖「连接 → 响应体读取完成」全阶段：
    // 仅用 `tokio::time::timeout` 包 `send()` 时，body 慢读会拖到客户端总超时（默认 30s）。
    let started = std::time::Instant::now();
    let resp = match tokio::time::timeout(cfg.timeout, rb.send()).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            tracing::warn!(
                site = %site.name,
                provider = %tokens.provider,
                sub = %tokens.sub,
                error = %e,
                "权限视图：resolve 调用失败"
            );
            resolve_metric(&site.name, "unavailable");
            return Err(ResolveFailure::Unavailable);
        }
        Err(_) => {
            tracing::warn!(
                site = %site.name,
                provider = %tokens.provider,
                sub = %tokens.sub,
                timeout_ms = cfg.timeout.as_millis(),
                "权限视图：resolve 超时"
            );
            resolve_metric(&site.name, "timeout");
            return Err(ResolveFailure::Timeout);
        }
    };
    metrics::histogram!(
        "bff_me_permissions_resolve_duration_seconds",
        "site" => site.name.clone()
    )
    .record(started.elapsed().as_secs_f64());

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        tracing::warn!(
            site = %site.name,
            provider = %tokens.provider,
            sub = %tokens.sub,
            %status,
            "权限视图：resolve 拒绝调用方身份"
        );
        resolve_metric(&site.name, "unauthorized");
        return Err(ResolveFailure::Unauthorized);
    }
    if !status.is_success() {
        tracing::warn!(
            site = %site.name,
            provider = %tokens.provider,
            sub = %tokens.sub,
            %status,
            "权限视图：resolve 返回非 2xx"
        );
        resolve_metric(&site.name, "upstream_status");
        return Err(ResolveFailure::UpstreamStatus);
    }
    let body = match crate::server::proxy::read_capped_body(resp, RESOLVE_MAX_BODY_BYTES).await {
        Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    site = %site.name,
                    provider = %tokens.provider,
                    sub = %tokens.sub,
                    error = %e,
                    "权限视图：resolve 响应非法 JSON"
                );
                resolve_metric(&site.name, "invalid_response");
                return Err(ResolveFailure::InvalidResponse);
            }
        },
        Err(e) => {
            tracing::warn!(
                site = %site.name,
                provider = %tokens.provider,
                sub = %tokens.sub,
                error = %e,
                "权限视图：resolve 响应超限"
            );
            resolve_metric(&site.name, "invalid_response");
            return Err(ResolveFailure::InvalidResponse);
        }
    };
    let Some(permissions) = map_resolve_response(&body, &cfg.response) else {
        tracing::warn!(
            site = %site.name,
            provider = %tokens.provider,
            sub = %tokens.sub,
            roles_path = %cfg.response.roles_path,
            permissions_path = %cfg.response.permissions_path,
            "权限视图：resolve 响应缺少配置的 roles/permissions 数组"
        );
        resolve_metric(&site.name, "invalid_response");
        return Err(ResolveFailure::InvalidResponse);
    };
    // TTL=0 = 不缓存（Redis 后端会把 0 钳到 1ms，必须在调用侧显式跳过）
    if cfg.cache_ttl > std::time::Duration::ZERO {
        if let Ok(bytes) = serde_json::to_vec(&permissions) {
            state.cache.set(&key, bytes, cfg.cache_ttl).await;
        }
    }
    resolve_metric(&site.name, "ok");
    Ok(permissions)
}

fn logged_out() -> serde_json::Value {
    serde_json::json!({"logged_in": false, "provider": null})
}

/// 权限缓存键：长度前缀消除分隔符歧义（provider 只校验非空、sub 由 IdP 控制，
/// 两者都可能含 `:`，平铺拼接会出现 `("iam:r","u1")` 与 `("iam","r:u1")` 同键的别名）。
fn permissions_cache_key(site: &str, provider: &str, sub: &str) -> String {
    format!(
        "bff:me:permissions:{site}:{}:{provider}:{}:{sub}",
        provider.len(),
        sub.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_id_token_claims_rejects_malformed() {
        assert!(decode_id_token_claims("not-a-jwt").is_none());
        assert!(decode_id_token_claims("a.!!!.c").is_none());
    }

    #[test]
    fn render_template_supports_documented_vars() {
        let ctx = ResolveCtx {
            site: "app1".into(),
            sub: "user-1".into(),
            provider: "iam".into(),
            access_token: "tok".into(),
            now: "2026-01-02T03:04:05Z".into(),
        };
        assert_eq!(
            render_template("{{site}}/{{provider}}/{{sub}}/{{now}}", &ctx).unwrap(),
            "app1/iam/user-1/2026-01-02T03:04:05Z"
        );
        assert_eq!(
            render_template("Bearer {{access_token}}", &ctx).unwrap(),
            "Bearer tok"
        );
        assert!(render_template("{{unknown}}", &ctx).is_none());
        assert!(render_template("{{unclosed", &ctx).is_none());
    }

    #[test]
    fn render_body_template_json_escapes_values() {
        let ctx = ResolveCtx {
            site: "app1".into(),
            sub: "u\"x".into(),
            provider: "iam".into(),
            access_token: "tok".into(),
            now: "2026-01-02T03:04:05Z".into(),
        };
        let body = render_body_template(r#"{"principal":"{{sub}}","as_of":"{{now}}"}"#, &ctx)
            .expect("渲染成功");
        let v: serde_json::Value = serde_json::from_str(&body).expect("仍是合法 JSON");
        assert_eq!(v["principal"], "u\"x");
        assert_eq!(v["as_of"], "2026-01-02T03:04:05Z");
        // access_token 不在 body 变量集合里
        assert!(render_body_template(r#"{"t":"{{access_token}}"}"#, &ctx).is_none());
    }

    #[test]
    fn json_path_walks_objects_and_arrays() {
        let v = serde_json::json!({"data": {"items": [{"roles": ["r"]}]}});
        assert_eq!(
            json_path(&v, "data.items.0.roles").unwrap(),
            &serde_json::json!(["r"])
        );
        assert!(json_path(&v, "data.items.1.roles").is_none());
        assert!(json_path(&v, "data..roles").is_none());
        assert!(json_path(&v, "missing").is_none());
    }

    #[test]
    fn map_resolve_response_uses_configured_paths_and_passthrough() {
        let body = serde_json::json!({
            "data": {
                "roles": ["admin"],
                "permissions": ["user:read", 7]
            },
            "sod_warnings": [{"code": "SOD-1"}]
        });
        let cfg = crate::config::MeResolveResponseConfig {
            roles_path: "data.roles".into(),
            permissions_path: "data.permissions".into(),
            passthrough: vec!["sod_warnings".into()],
        };
        let v = map_resolve_response(&body, &cfg).expect("映射成功");
        assert_eq!(v["roles"], serde_json::json!(["admin"]));
        assert_eq!(v["permissions"], serde_json::json!(["user:read"]));
        assert_eq!(v["sod_warnings"], serde_json::json!([{"code": "SOD-1"}]));

        // 两个路径都没命中 → 结构非法
        assert!(map_resolve_response(&serde_json::json!({"roles": []}), &cfg).is_none());
    }

    #[test]
    fn identity_fields_keeps_only_standard_string_claims() {
        let claims = serde_json::json!({
            "sub": "ignored",
            "preferred_username": "alice",
            "name": "Alice",
            "email": "alice@example.test",
            "groups": ["internal"],
            "nonce": "n",
            "iat": 1
        });
        let v = identity_fields("user-1", Some(&claims));
        assert_eq!(v["sub"], "user-1", "sub 以会话为准");
        assert_eq!(v["preferred_username"], "alice");
        assert_eq!(v["name"], "Alice");
        assert_eq!(v["email"], "alice@example.test");
        assert!(v.get("groups").is_none(), "非白名单 claims 不得透传");
        assert!(v.get("nonce").is_none());
        assert!(v.get("iat").is_none());
    }

    #[test]
    fn permissions_fields_filters_and_defaults() {
        let v = permissions_fields(&serde_json::json!({
            "roles": ["admin", 1],
            "permissions": ["user:read"]
        }))
        .expect("roles/permissions 数组应被接受");
        assert_eq!(v["roles"], serde_json::json!(["admin"]));
        assert_eq!(v["permissions"], serde_json::json!(["user:read"]));

        // 只有一个键：另一个补空数组
        let v = permissions_fields(&serde_json::json!({"roles": []})).unwrap();
        assert_eq!(v["permissions"], serde_json::json!([]));

        // 结构非法：按键存在但非数组 → 整体拒绝（不输出半截字段）
        assert!(permissions_fields(&serde_json::json!({"roles": "admin"})).is_none());
        assert!(permissions_fields(&serde_json::json!({})).is_none());
        assert!(permissions_fields(&serde_json::json!({"other": []})).is_none());
    }

    #[test]
    fn permissions_cache_key_is_unambiguous() {
        // 分隔符别名：provider 含 `:` 时不得与「更短 provider + 含 `:` 的 sub」碰撞
        assert_ne!(
            permissions_cache_key("app1", "iam:r", "u1"),
            permissions_cache_key("app1", "iam", "r:u1")
        );
        // 站点维度必须参与键
        assert_ne!(
            permissions_cache_key("app1", "p", "x"),
            permissions_cache_key("app2", "p", "x")
        );
    }

    #[test]
    fn identity_fields_with_exposes_allowlisted_claims_only() {
        let claims = serde_json::json!({
            "tenant_id": "tenant-9",
            "roles": ["admin", "auditor"],
            "org_id": 42,
            "mfa": true,
            "nested": {"a": 1},
            "mixed": ["ok", 1],
            "empty": [],
            "nonce": "n-1",
            "email": "a@example.test"
        });
        let allow: Vec<String> = [
            "tenant_id",
            "roles",
            "org_id",
            "mfa",
            "nested",
            "mixed",
            "empty",
            "nonce",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let v = identity_fields_with("app1", "user-1", Some(&claims), &allow);
        assert_eq!(v["sub"], "user-1");
        assert_eq!(v["email"], "a@example.test");
        assert_eq!(v["tenant_id"], "tenant-9");
        assert_eq!(v["roles"], serde_json::json!(["admin", "auditor"]));
        assert_eq!(v["org_id"], 42);
        assert_eq!(v["mfa"], true);
        assert!(v.get("nested").is_none(), "对象类型不透传");
        assert!(v.get("mixed").is_none(), "混合数组不透传");
        assert!(v.get("empty").is_none(), "空数组不透传");
        assert!(v.get("nonce").is_none(), "敏感 claim 即使配置也拒绝");
    }

    #[test]
    fn identity_fields_with_ignores_claims_absent_from_token() {
        let claims = serde_json::json!({"sub": "user-1"});
        let allow = vec!["tenant_id".to_string()];
        let v = identity_fields_with("app1", "user-1", Some(&claims), &allow);
        assert!(v.get("tenant_id").is_none());
        assert_eq!(v["sub"], "user-1");
    }

    #[test]
    fn identity_fields_with_enforces_size_cap_without_dropping_identity() {
        let huge = "x".repeat(crate::config::ME_IDENTITY_MAX_BYTES);
        let claims = serde_json::json!({"huge": huge, "tenant_id": "t-1"});
        let allow = vec!["huge".to_string(), "tenant_id".to_string()];
        let v = identity_fields_with("app1", "user-1", Some(&claims), &allow);
        assert!(v.get("huge").is_none(), "超限 claim 不得透传");
        assert_eq!(v["tenant_id"], "t-1");
        assert_eq!(v["sub"], "user-1");
        assert!(serde_json::to_vec(&v).unwrap().len() <= crate::config::ME_IDENTITY_MAX_BYTES);
    }

    #[test]
    fn identity_fields_omits_missing_or_non_string_claims() {
        let claims = serde_json::json!({"name": 42, "email": {"a": 1}});
        let v = identity_fields("user-1", Some(&claims));
        assert_eq!(v.as_object().unwrap().len(), 1);
        assert_eq!(v["sub"], "user-1");
    }
}
