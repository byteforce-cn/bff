//! OIDC 登录 / 回调 / 登出处理器，以及令牌刷新（分布式锁防惊群）。
use crate::config::OidcProviderConfig;
use crate::oidc::tokens::{flow_key, now_unix, session_key, StoredTokens};
use crate::state::{AppState, SessionInfo};
use crate::utils::AppError;
use anyhow::Context;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use openidconnect::core::{CoreAuthenticationFlow, CoreTokenResponse};
use openidconnect::{
    AuthorizationCode, CsrfToken, Nonce, OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier,
    RefreshToken, Scope,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tower_sessions::Session;

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    provider: Option<String>,
    redirect: Option<String>,
    popup: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    provider: Option<String>,
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// 授权流程暂存（state / nonce / PKCE verifier）
#[derive(Debug, Serialize, Deserialize)]
struct AuthFlow {
    state: String,
    nonce: String,
    pkce_verifier: String,
    redirect_after_login: Option<String>,
    popup: bool,
}

/// 选择 provider：未指定且仅配置一个时取默认值。
pub fn select_provider(state: &AppState, id: Option<&str>) -> Result<OidcProviderConfig, AppError> {
    let cfg = state.cfg();
    match id {
        Some(id) => cfg
            .oidc
            .providers
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| AppError::not_found(format!("OIDC provider 不存在: {}", id))),
        None => match cfg.oidc.providers.as_slice() {
            [single] => Ok(single.clone()),
            [] => Err(AppError::bad_request("未配置任何 OIDC provider")),
            _ => Err(AppError::bad_request(
                "存在多个 provider，请通过 ?provider= 指定",
            )),
        },
    }
}

/// 校验 redirect 参数：只允许**同源绝对路径**。
///
/// S1：原实现 `starts_with('/') && !starts_with("//")` 可被 `\/\evil.com` 绕过
/// （浏览器将 `\` 归一为 `/` → 实际跳转到外站）。改为用 URL 解析器按同源校验：
/// - 拒绝控制字符；
/// - 以校验基准（dummy origin）解析后，结果 origin 必须仍为基准本身，
///   因此 `//evil.com`、`/\evil.com`、`https://evil.com` 等一律拒绝。
pub fn validate_redirect(redirect: &str) -> bool {
    if redirect.is_empty() || redirect.chars().any(|c| c.is_control()) {
        return false;
    }
    // S1：路径中不允许出现裸反斜杠（浏览器会将其归一为 `/`，形成 `//evil.com` 类外链）
    if redirect.contains('\\') {
        return false;
    }
    // 保守拒绝百分号编码的反斜杠：即使浏览器不会二次解码，也避免任何归一化歧义
    if redirect.to_ascii_lowercase().contains("%5c") {
        return false;
    }
    // 必须是站内绝对路径（相对路径会按当前页面深度解析，语义不可控）
    if !redirect.starts_with('/') {
        return false;
    }
    let base = match url::Url::parse("http://bff-redirect-check.invalid/") {
        Ok(b) => b,
        Err(_) => return false,
    };
    match base.join(redirect) {
        Ok(u) => {
            u.scheme() == "http"
                && u.host_str() == Some("bff-redirect-check.invalid")
                && u.path().starts_with('/')
        }
        Err(_) => false,
    }
}

/// 推导本服务对外 base_url（P0-2）。
///
/// 1. 配置了 `server.public_base_url` → **一律使用它，完全不信任 Host 头**；
/// 2. 否则回退 `Host`（+ 可信 `X-Forwarded-Proto`），且：
///    - `server.trusted_hosts` 非空时，Host 必须命中白名单；
///    - 为空时仅允许 loopback Host（开发/测试），其余拒绝。
///
/// 拒绝而非静默回退：避免“回调地址与实际入口不符”在生产变成难排查的登录故障。
fn base_url_from(headers: &HeaderMap, state: &AppState) -> Result<String, AppError> {
    let cfg = state.cfg();
    if let Some(base) = &cfg.server.public_base_url {
        return Ok(base.trim_end_matches('/').to_string());
    }

    if let Some(host) = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
    {
        let trusted = if cfg.server.trusted_hosts.is_empty() {
            is_loopback_host(host)
        } else {
            cfg.server
                .trusted_hosts
                .iter()
                .any(|t| t.eq_ignore_ascii_case(host.trim()))
        };
        if trusted {
            let proto = headers
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .map(str::trim)
                .filter(|p| *p == "https")
                .unwrap_or("http");
            return Ok(format!("{}://{}", proto, host.trim()));
        }
        return Err(AppError::bad_request(
            "Host 不受信任：请配置 server.public_base_url 或 server.trusted_hosts",
        ));
    }

    // 无 Host 头（非常规协议）：回退本机（仅开发/测试）
    Ok(format!("http://127.0.0.1:{}", cfg.server.business_port))
}

/// 与请求无关的规范 base_url（后台刷新等非请求路径使用，P0-2）。
///
/// `public_base_url` 优先；否则回退本机地址（仅用于 client 缓存键与 discovery，
/// 不参与 redirect_uri 下发）。
pub fn canonical_base_url(state: &AppState) -> String {
    let cfg = state.cfg();
    cfg.server
        .public_base_url
        .clone()
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", cfg.server.business_port))
        .trim_end_matches('/')
        .to_string()
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim();
    let hostname = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or_default()
    };
    matches!(hostname, "localhost" | "127.0.0.1" | "::1")
}

/// 计算授权请求的附加 scope 列表。
///
/// `openidconnect` 的 `authorize_url` 已隐式注入 `openid`（crate 源码 lib.rs），
/// 若配置再写一次 `openid` 会生成 `scope=openid+openid+...`；
/// 部分严格实现的 IdP 会以 `invalid_scope` 拒绝（Keycloak 契约验证实测确认）。
/// 此处按大小写不敏感去重（视为已含 openid），并保留其余 scope 的配置顺序。
fn authorize_scopes(config_scopes: &[String]) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen.insert("openid".to_string());
    let mut out = Vec::new();
    for scope in config_scopes {
        let s = scope.trim();
        if !s.is_empty() && seen.insert(s.to_ascii_lowercase()) {
            out.push(s.to_string());
        }
    }
    out
}

/// GET /login — 发起授权码 + PKCE 流程
pub async fn login(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Query(q): Query<LoginQuery>,
) -> Result<Response, AppError> {
    let provider = select_provider(&state, q.provider.as_deref())?;
    let base_url = base_url_from(&headers, &state)?;
    let client = state
        .oidc_clients
        .get(&provider, &base_url)
        .await
        .map_err(|e| {
            // S12：对外统一文案；内部细节（含拓扑/元数据错误）仅进日志
            tracing::error!(provider = %provider.id, error = %e, "OIDC client 构建失败");
            AppError::bad_gateway("身份服务暂不可用（discovery 失败）")
        })?;

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let mut req = client.authorize_url(
        CoreAuthenticationFlow::AuthorizationCode,
        CsrfToken::new_random,
        Nonce::new_random,
    );
    for scope in authorize_scopes(&provider.scopes) {
        req = req.add_scope(Scope::new(scope));
    }
    let (auth_url, csrf, nonce) = req.set_pkce_challenge(pkce_challenge).url();

    let redirect = q
        .redirect
        .as_deref()
        .filter(|r| validate_redirect(r))
        .map(|r| r.to_string());
    let popup = q.popup.unwrap_or(false);

    let flow = AuthFlow {
        state: csrf.secret().clone(),
        nonce: nonce.secret().clone(),
        pkce_verifier: pkce_verifier.secret().clone(),
        redirect_after_login: redirect,
        popup,
    };
    session
        .insert(&flow_key(&provider.id), flow)
        .await
        .context("写入 session 失败")?;

    Ok(Redirect::to(auth_url.as_str()).into_response())
}

/// GET /auth/callback — IdP 回调：换码、验签、建会话
pub async fn callback(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    if let Some(err) = q.error {
        return Err(AppError::unauthorized(format!(
            "IdP 返回错误: {} {}",
            err,
            q.error_description.unwrap_or_default()
        )));
    }
    let provider = select_provider(&state, q.provider.as_deref())?;
    let code = q.code.ok_or_else(|| AppError::bad_request("缺少 code"))?;
    let state_param = q.state.ok_or_else(|| AppError::bad_request("缺少 state"))?;

    let flow: AuthFlow = session
        .get(&flow_key(&provider.id))
        .await
        .context("读取 session 失败")?
        .ok_or_else(|| AppError::unauthorized("授权流程不存在或已过期"))?;
    if flow.state != state_param {
        return Err(AppError::unauthorized("state 校验失败（CSRF 防护）"));
    }

    let base_url = base_url_from(&headers, &state)?;
    let client = state
        .oidc_clients
        .get(&provider, &base_url)
        .await
        .map_err(|e| {
            tracing::error!(provider = %provider.id, error = %e, "OIDC client 构建失败（callback）");
            AppError::bad_gateway("身份服务暂不可用（discovery 失败）")
        })?;

    let token_response: CoreTokenResponse = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(PkceCodeVerifier::new(flow.pkce_verifier))
        .request_async(crate::oidc::http_client::client_fn(state.oidc_http.clone()))
        .await
        .map_err(|e| {
            // S12：不向调用方回显 IdP 内部细节
            tracing::warn!(error = %e, "code 换 token 失败");
            AppError::unauthorized("登录失败：令牌交换未完成，请重试")
        })?;

    let sub = verify_id_token(&state, &provider, &base_url, &token_response, &flow.nonce).await?;

    // S2：登录成功（权限提升）后轮换 session id，防会话固定攻击。
    // tower-sessions 的 cycle_id 会生成新 ID 并删除旧记录；
    // 之后的令牌写入/会话登记均基于新 ID。
    session.cycle_id().await.context("轮换 session id 失败")?;

    let stored = StoredTokens::new(
        &provider.id,
        &sub,
        token_response.access_token().secret(),
        token_response.refresh_token().map(|t| t.secret().as_str()),
        token_response
            .extra_fields()
            .id_token()
            .map(|t| t.to_string())
            .as_deref(),
        token_response
            .expires_in()
            .map(|d| d.as_secs() as i64)
            .unwrap_or(3600),
    )
    .map_err(|e| AppError::internal(e.to_string()))?;

    session
        .insert(&session_key(&provider.id), stored)
        .await
        .context("写入 session 失败")?;
    session
        .insert("oidc:current_provider", &provider.id)
        .await
        .context("写入 session 失败")?;
    session.remove_value(&flow_key(&provider.id)).await.ok();

    // tower-sessions 的 `cycle_id()` 会把内部 session id 置空（crate 语义：
    // 保存时由 store.create 重新分配），必须显式 save 后才能读到新 id；
    // 否则 `register_session` 的 `session.id()` 为 None → 管理端会话列表永远为空
    // （Keycloak 真实 IdP 契约验证实测发现，S2 会话轮换的回归）。
    session.save().await.context("保存会话失败")?;

    register_session(&state, &session, &provider.id, &sub).await;
    metrics::counter!("bff_oidc_login_total", "provider" => provider.id.clone()).increment(1);

    // popup 模式：返回自关闭 HTML 页面，通知 opener
    if flow.popup {
        return Ok(Response::builder()
            .status(200)
            .header("content-type", "text/html; charset=utf-8")
            .body(Body::from(POPUP_CLOSE_HTML))
            .unwrap());
    }

    let target = flow.redirect_after_login.unwrap_or_else(|| "/".into());
    Ok(Redirect::to(&target).into_response())
}

/// GET /logout — 清除本地会话并登出 IdP（RP-Initiated Logout）
pub async fn logout(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Query(q): Query<LoginQuery>,
) -> Result<Response, AppError> {
    let provider = select_provider(&state, q.provider.as_deref()).ok();

    // 尝试取出 id_token 作为 id_token_hint（在清除 session 之前）
    let id_token_hint = match &provider {
        Some(p) => session
            .get::<StoredTokens>(&session_key(&p.id))
            .await
            .ok()
            .flatten()
            .and_then(|stored| stored.id_token().ok().flatten()),
        None => None,
    };

    // 清除 BFF 本地会话
    let sid = session.id().map(|id| id.to_string());
    if let Some(p) = &provider {
        session.remove_value(&session_key(&p.id)).await.ok();
    }
    unregister_session(&state, &session).await;
    session.flush().await.ok();

    // R17：登出必须同时吊销该会话的 token exchange 缓存，
    // 否则已登出会话换来的上游令牌在 TTL 内仍可被复用（containment 缺口）。
    if let Some(sid) = &sid {
        let cleared = crate::server::token_exchange::clear_session_cache(&state, sid).await;
        if cleared > 0 {
            tracing::info!(session_id = %sid, cleared, "登出清理 token exchange 缓存");
        }
    }

    // F12：登出端点以 discovery 的 `end_session_endpoint` 为准
    // （原实现硬编码 Spring AS 的 `/connect/logout`，换 IdP 即失效）。
    match &provider {
        Some(p) => {
            let base_url = base_url_from(&headers, &state)?;
            let post_logout_redirect = format!("{}/", base_url.trim_end_matches('/'));
            match state.oidc_clients.end_session_endpoint(p).await {
                Some(endpoint) => {
                    let mut logout_url = format!(
                        "{}?post_logout_redirect_uri={}",
                        endpoint,
                        urlencoding(&post_logout_redirect)
                    );
                    if let Some(hint) = &id_token_hint {
                        logout_url.push_str(&format!("&id_token_hint={}", urlencoding(hint)));
                    }
                    logout_url.push_str(&format!("&client_id={}", urlencoding(&p.client_id)));
                    tracing::info!(
                        provider = %p.id,
                        end_session_endpoint = %endpoint,
                        has_id_token_hint = id_token_hint.is_some(),
                        "RP-Initiated Logout: 重定向到 IdP"
                    );
                    Ok(Redirect::to(&logout_url).into_response())
                }
                None => {
                    tracing::warn!(
                        provider = %p.id,
                        "IdP discovery 未提供 end_session_endpoint，仅清除本地会话"
                    );
                    Ok(Redirect::to("/").into_response())
                }
            }
        }
        None => {
            tracing::info!("无 provider，仅清除本地 session");
            Ok(Redirect::to("/").into_response())
        }
    }
}

/// Popup 模式回调完成后返回的自关闭 HTML 页面。
/// 通过 postMessage 通知 opener，然后自动关闭窗口。
const POPUP_CLOSE_HTML: &str = r#"<!DOCTYPE html>
<html><body>
<script>
try { window.opener && window.opener.postMessage('oidc-done', window.location.origin); } catch(e) {}
window.close();
</script>
<p>登录完成，窗口即将关闭...</p>
</body></html>"#;

fn urlencoding(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}

/// 校验 ID Token 并返回 sub。`insecure_skip_id_token_verification` 仅用于开发/测试。
/// 签名校验失败时自动重试一次（重新发现 provider 元数据，处理 IdP 重启导致的密钥轮换）。
async fn verify_id_token(
    state: &AppState,
    provider: &OidcProviderConfig,
    base_url: &str,
    token_response: &CoreTokenResponse,
    expected_nonce: &str,
) -> Result<String, AppError> {
    let id_token = token_response
        .extra_fields()
        .id_token()
        .ok_or_else(|| AppError::unauthorized("响应缺少 id_token"))?;

    if provider.insecure_skip_id_token_verification {
        let claims = decode_jwt_payload_unverified(&id_token.to_string())?;
        let nonce = claims
            .get("nonce")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if nonce != expected_nonce {
            return Err(AppError::unauthorized("nonce 校验失败"));
        }
        let exp = claims.get("exp").and_then(|v| v.as_i64()).unwrap_or(0);
        if exp < now_unix() {
            return Err(AppError::unauthorized("id_token 已过期"));
        }
        let sub = claims
            .get("sub")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::unauthorized("id_token 缺少 sub"))?;
        return Ok(sub.to_string());
    }

    // 第一次尝试：使用当前缓存的 OIDC 客户端校验
    match try_verify_with_client(state, provider, base_url, id_token, expected_nonce).await {
        Ok(sub) => Ok(sub),
        Err(first_err) => {
            tracing::warn!(
                provider = %provider.id,
                error = %first_err,
                "id_token 首次校验失败，失效缓存后重试"
            );
            // 失效缓存：处理 IdP 重启后密钥轮换
            state.oidc_clients.invalidate(&provider.id).await;
            // 重试
            try_verify_with_client(state, provider, base_url, id_token, expected_nonce)
                .await
                .map_err(|_| AppError::unauthorized(format!("id_token 校验失败: {}", first_err)))
        }
    }
}

async fn try_verify_with_client(
    state: &AppState,
    provider: &OidcProviderConfig,
    base_url: &str,
    id_token: &openidconnect::core::CoreIdToken,
    expected_nonce: &str,
) -> Result<String, AppError> {
    let client = state
        .oidc_clients
        .get(provider, base_url)
        .await
        .map_err(|e| AppError::bad_gateway(e.to_string()))?;
    let claims = id_token
        .claims(
            &client.id_token_verifier(),
            &Nonce::new(expected_nonce.to_string()),
        )
        .map_err(|e| AppError::unauthorized(format!("id_token 校验失败: {}", e)))?;
    Ok(claims.subject().to_string())
}

/// 不验签地解析 JWT payload（仅 insecure 模式）。
fn decode_jwt_payload_unverified(jwt: &str) -> Result<serde_json::Value, AppError> {
    let payload = jwt
        .split('.')
        .nth(1)
        .ok_or_else(|| AppError::unauthorized("id_token 格式非法"))?;
    let bytes =
        base64::engine::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, payload)
            .map_err(|e| AppError::unauthorized(format!("id_token 解码失败: {}", e)))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| AppError::unauthorized(format!("id_token claims 解析失败: {}", e)))
}

/// 令牌刷新（LockProvider 防惊群）。成功返回新的 StoredTokens。
/// 持锁后会检查 token 是否仍在 skew 窗口，已刷新则直接返回。
pub async fn try_refresh(
    state: &AppState,
    session: &Session,
    tokens: &StoredTokens,
) -> Result<Option<StoredTokens>, AppError> {
    let provider = match select_provider(state, Some(&tokens.provider)) {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let Some(refresh_token) = tokens.refresh_token().ok().flatten() else {
        return Ok(None);
    };
    let sid = session
        .id()
        .map(|id| id.to_string())
        .unwrap_or_else(|| "unknown".into());
    let lock_key = format!("bff:refresh_lock:{}", sid);
    let Some(guard) = state
        .lock
        .acquire(
            &lock_key,
            Duration::from_millis(500),
            Duration::from_secs(5),
        )
        .await
    else {
        // 其他请求正在刷新：等待后从 store 重读最新会话
        tokio::time::sleep(Duration::from_millis(100)).await;
        session.load().await.ok();
        let fresh: Option<StoredTokens> =
            session.get(&session_key(&provider.id)).await.ok().flatten();
        return Ok(fresh);
    };

    // 持锁后从 store 重读：可能已被并发请求刷新（内存副本可能是旧的）
    session.load().await.ok();
    let current: Option<StoredTokens> =
        session.get(&session_key(&provider.id)).await.ok().flatten();
    if let Some(cur) = &current {
        if !cur.is_expiring(provider.refresh_skew_secs) {
            guard.release().await;
            return Ok(Some(cur.clone()));
        }
    }

    let result = do_refresh(state, &provider, &tokens.sub, refresh_token).await;
    match result {
        Ok(new_tokens) => {
            if session
                .insert(&session_key(&provider.id), &new_tokens)
                .await
                .is_ok()
            {
                // 立即持久化，让持锁等待的并发请求读到新令牌
                session.save().await.ok();
                metrics::counter!("bff_oidc_refresh_total", "provider" => provider.id.clone())
                    .increment(1);
            }
            guard.release().await;
            Ok(Some(new_tokens))
        }
        Err(e) => {
            guard.release().await;
            Err(AppError::unauthorized(format!("令牌刷新失败: {}", e)))
        }
    }
}

/// 强制刷新（供代理层 401 重试使用）。跳过 is_expiring 检查，只要 upstream 拒绝了 token 就刷新。
pub async fn force_refresh(
    state: &AppState,
    session: &Session,
    tokens: &StoredTokens,
) -> Result<Option<StoredTokens>, AppError> {
    let provider = match select_provider(state, Some(&tokens.provider)) {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let Some(refresh_token) = tokens.refresh_token().ok().flatten() else {
        return Ok(None);
    };
    let sid = session
        .id()
        .map(|id| id.to_string())
        .unwrap_or_else(|| "unknown".into());
    let lock_key = format!("bff:refresh_lock:{}", sid);
    let Some(guard) = state
        .lock
        .acquire(
            &lock_key,
            Duration::from_millis(500),
            Duration::from_secs(5),
        )
        .await
    else {
        // 其他请求正在刷新：等待后从 store 重读
        tokio::time::sleep(Duration::from_millis(100)).await;
        session.load().await.ok();
        return Ok(session.get(&session_key(&provider.id)).await.ok().flatten());
    };

    // force: 不检查 is_expiring，直接刷新
    let result = do_refresh(state, &provider, &tokens.sub, refresh_token).await;
    match result {
        Ok(new_tokens) => {
            session
                .insert(&session_key(&provider.id), &new_tokens)
                .await
                .ok();
            session.save().await.ok();
            metrics::counter!("bff_oidc_refresh_total", "provider" => provider.id.clone())
                .increment(1);
            guard.release().await;
            Ok(Some(new_tokens))
        }
        Err(e) => {
            guard.release().await;
            Err(AppError::unauthorized(format!("令牌刷新失败: {}", e)))
        }
    }
}

async fn do_refresh(
    state: &AppState,
    provider: &OidcProviderConfig,
    sub: &str,
    refresh_token: String,
) -> anyhow::Result<StoredTokens> {
    // P0-2：refresh 路径不再硬编码 127.0.0.1，改用与 public_base_url 对齐的规范 base；
    // 叠加 OidcClientManager 的 (provider_id, base_url) 缓存键，
    // 彻底消除“后台刷新把整机 redirect_uri 钉死”的跨用户污染。
    let base_url = canonical_base_url(state);
    let client = state.oidc_clients.get(provider, &base_url).await?;
    let resp: CoreTokenResponse = client
        .exchange_refresh_token(&RefreshToken::new(refresh_token))
        .request_async(crate::oidc::http_client::client_fn(state.oidc_http.clone()))
        .await
        .map_err(|e| anyhow::anyhow!("refresh_token 交换失败: {}", e))?;
    let stored = StoredTokens::new(
        &provider.id,
        sub,
        resp.access_token().secret(),
        resp.refresh_token().map(|t| t.secret().as_str()),
        resp.extra_fields()
            .id_token()
            .map(|t| t.to_string())
            .as_deref(),
        resp.expires_in()
            .map(|d| d.as_secs() as i64)
            .unwrap_or(3600),
    )?;
    Ok(stored)
}

pub async fn register_session(state: &AppState, session: &Session, provider: &str, sub: &str) {
    let Some(id) = session.id() else {
        // 防御未来回归：cycle_id 后未 save（或 crate 语义变化）时 id 为 None，
        // 静默跳过会导致管理端会话列表失真且难以定位。
        tracing::warn!(
            provider,
            sub,
            "会话索引登记跳过：tower-sessions 尚未分配 session id（cycle_id 后缺 save？）"
        );
        return;
    };
    let now = now_unix();
    state.sessions.write().await.insert(
        id.to_string(),
        SessionInfo {
            id: id.to_string(),
            provider: provider.into(),
            sub: sub.into(),
            created_at: now,
            last_seen: now,
        },
    );
}

pub async fn unregister_session(state: &AppState, session: &Session) {
    if let Some(id) = session.id() {
        state.sessions.write().await.remove(&id.to_string());
    }
}

/// 供代理层取当前会话的明文 access token。
pub async fn current_access_token(session: &Session) -> Option<String> {
    let tokens = current_tokens(session).await?;
    tokens.access_token().ok()
}

/// 供中间件读取当前会话的令牌（含 provider 键）。
pub async fn current_tokens(session: &Session) -> Option<StoredTokens> {
    let provider: String = session.get("oidc:current_provider").await.ok().flatten()?;
    session.get(&session_key(&provider)).await.ok().flatten()
}

#[cfg(test)]
mod redirect_tests {
    use super::{authorize_scopes, validate_redirect};

    #[test]
    fn allows_same_origin_paths() {
        assert!(validate_redirect("/"));
        assert!(validate_redirect("/dashboard"));
        assert!(validate_redirect("/a/b?x=1&y=2"));
        assert!(validate_redirect("/path#frag"));
    }

    #[test]
    fn rejects_open_redirect_shapes() {
        // S1：浏览器把 `\` 归一为 `/`，`/\evil.com` 实为协议相对外链
        assert!(!validate_redirect("/\\evil.com"));
        assert!(!validate_redirect("\\evil.com"));
        assert!(!validate_redirect("\\\\evil.com"));
        // 协议相对 / 绝对外链
        assert!(!validate_redirect("//evil.com"));
        assert!(!validate_redirect("https://evil.com"));
        assert!(!validate_redirect("http://evil.com/x"));
        assert!(!validate_redirect("javascript:alert(1)"));
        // 控制字符
        assert!(!validate_redirect("/ok\r\nLocation: https://evil.com"));
        assert!(!validate_redirect(""));
        // 百分号编码的反斜杠（浏览器不会二次解码，保守拒绝）
        assert!(!validate_redirect("/%5Cevil.com"));
    }

    #[test]
    fn scopes_dedup_openid_and_duplicates() {
        let cfg = vec![
            "openid".to_string(),
            "profile".to_string(),
            "EMAIL".to_string(),
            "email".to_string(),
            " openid ".to_string(),
            "".to_string(),
        ];
        // openid 由 crate 隐式注入 → 配置中的全部变体被跳过；
        // 其余 scope 大小写不敏感去重且保持顺序。
        assert_eq!(authorize_scopes(&cfg), vec!["profile", "EMAIL"]);
        // 配置本身不含 openid 时不会额外注入（由 crate 负责）
        assert_eq!(authorize_scopes(&["profile".into()]), vec!["profile"]);
        assert!(authorize_scopes(&[]).is_empty());
    }
}
