//! `GET /api/me` — 当前用户与会话视图（同源只读，绝不含任何令牌）。
//!
//! 身份取会话中已存的 ID Token claims（本地解码，零网络）；权限视图
//! （`me.permissions`，缺省关闭）由 `resolve_permissions` 追加。
use crate::config::ResolvedMePermissions;
use crate::oidc::tokens::StoredTokens;
use crate::site::{SiteHandle, SiteView};
use crate::state::AppState;
use axum::extract::{Extension, State};
use axum::response::Json;
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

/// 白名单身份字段：`sub`（以会话为准）+ ID Token 中的三个标准字符串 claim。
/// 其它 claims（groups / nonce / iat …）一律不透传。
pub(crate) fn identity_fields(sub: &str, claims: Option<&serde_json::Value>) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert("sub".into(), serde_json::Value::String(sub.to_string()));
    for key in ["preferred_username", "name", "email"] {
        if let Some(v) = claims.and_then(|c| c.get(key)).and_then(|v| v.as_str()) {
            out.insert(key.into(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::Value::Object(out)
}

/// GET /api/me — 站点感知：登录态与身份按站点当前 provider 解析（与 `/api/session` 同源）。
pub async fn me_handler(
    State(state): State<AppState>,
    Extension(handle): Extension<Arc<SiteHandle>>,
    session: Session,
) -> Json<serde_json::Value> {
    let view = state.site_view(&handle.name).expect("site view");
    let Some(tokens) = view.current_tokens(&session).await else {
        return Json(logged_out());
    };
    let id_token = tokens.id_token().ok().flatten();
    let claims = id_token.as_deref().and_then(decode_id_token_claims);
    let mut body = identity_fields(&tokens.sub, claims.as_ref());
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
        if let Some(permissions) = resolve_permissions(&state, &view, &tokens, cfg).await {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("permissions".into(), permissions);
            }
        }
    }
    Json(body)
}

/// 权限视图解析结果校验：`roles` / `permissions` 至少出现一个且必须是字符串数组；
/// 结构非法返回 `None`（整体拒绝，不输出半截字段）。
pub(crate) fn permissions_fields(resolve_body: &serde_json::Value) -> Option<serde_json::Value> {
    fn strings(v: &serde_json::Value) -> Option<Vec<String>> {
        let arr = v.as_array()?;
        Some(
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
        )
    }
    let has_any = resolve_body.get("roles").is_some() || resolve_body.get("permissions").is_some();
    if !has_any {
        return None;
    }
    let roles = match resolve_body.get("roles") {
        Some(v) => strings(v)?,
        None => Vec::new(),
    };
    let permissions = match resolve_body.get("permissions") {
        Some(v) => strings(v)?,
        None => Vec::new(),
    };
    Some(serde_json::json!({"roles": roles, "permissions": permissions}))
}

/// 调用下游授权权威解析权限；任何失败/超时/结构非法都返回 `None`（降级为身份视图）。
/// resolve 响应体上限：权限清单应远小于此；超出即按失败降级（防下游异常/被攻陷时内存膨胀）。
const RESOLVE_MAX_BODY_BYTES: usize = 64 * 1024;

/// 成功结果按 `site + provider + sub` 缓存，TTL 见 `me.permissions.cache_ttl`。
async fn resolve_permissions(
    state: &AppState,
    site: &SiteView,
    tokens: &StoredTokens,
    cfg: &ResolvedMePermissions,
) -> Option<serde_json::Value> {
    // 长度前缀消除分隔符歧义（provider 只校验非空、sub 由 IdP 控制，两者都可能含 `:`）。
    let key = permissions_cache_key(&site.name, &tokens.provider, &tokens.sub);
    if let Some(bytes) = state.cache.get(&key).await {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            return Some(v);
        }
    }
    let access_token = match tokens.access_token() {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "权限视图：access token 解密失败，降级");
            return None;
        }
    };
    // `RequestBuilder::timeout` 覆盖「连接 → 响应体读取完成」全阶段：
    // 仅用 `tokio::time::timeout` 包 `send()` 时，body 慢读会拖到客户端总超时（默认 30s）。
    let request = state
        .http
        .post(&cfg.resolve_url)
        .timeout(cfg.timeout)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {access_token}"),
        )
        .json(&serde_json::json!({"sub": tokens.sub, "provider": tokens.provider}));
    let resp = match tokio::time::timeout(cfg.timeout, request.send()).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "权限视图：resolve 调用失败，降级");
            return None;
        }
        Err(_) => {
            tracing::warn!(
                timeout_ms = cfg.timeout.as_millis(),
                "权限视图：resolve 超时，降级"
            );
            return None;
        }
    };
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(%status, "权限视图：resolve 返回非 2xx，降级");
        return None;
    }
    let body = match crate::server::proxy::read_capped_body(resp, RESOLVE_MAX_BODY_BYTES).await {
        Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "权限视图：resolve 响应非法 JSON，降级");
                return None;
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "权限视图：resolve 响应超限，降级");
            return None;
        }
    };
    let Some(permissions) = permissions_fields(&body) else {
        tracing::warn!("权限视图：resolve 响应缺少 roles/permissions 数组，降级");
        return None;
    };
    // TTL=0 = 不缓存（Redis 后端会把 0 钳到 1ms，必须在调用侧显式跳过）
    if cfg.cache_ttl > std::time::Duration::ZERO {
        if let Ok(bytes) = serde_json::to_vec(&permissions) {
            state.cache.set(&key, bytes, cfg.cache_ttl).await;
        }
    }
    Some(permissions)
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
    fn identity_fields_omits_missing_or_non_string_claims() {
        let claims = serde_json::json!({"name": 42, "email": {"a": 1}});
        let v = identity_fields("user-1", Some(&claims));
        assert_eq!(v.as_object().unwrap().len(), 1);
        assert_eq!(v["sub"], "user-1");
    }
}
