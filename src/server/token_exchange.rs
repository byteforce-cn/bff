//! RFC 8693 Token Exchange：以会话 access token 为 `subject_token`，向授权服务器
//! 换取**面向上游资源**的 access token，再注入代理请求。
//!
//! 本模块是代理路由的**通用前置编排能力**，不绑定任何具体业务：
//! - `exchange`：执行一次交换（不含缓存）；
//! - `resolve`：面向代理的入口，含缓存（键 = session + 配置指纹 + subject 指纹）、
//!   single-flight 防惊群、`invalid_grant`/`invalid_token` 时「刷新会话后重试一次」。
//!
//! 语义参考：docs/token-exchange-rfc8693.md（§3/§6/§7）。

use crate::config::{TokenExchangeAuthMethod, TokenExchangeConfig};
use crate::oidc::handlers::{current_access_token, current_tokens, force_refresh};
use crate::state::AppState;
use crate::utils::AppError;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tower_sessions::Session;

/// 缓存键前缀（与 `bff:refresh_lock:` 等现有命名一致）。
const CACHE_PREFIX: &str = "bff:token_exchange:";
/// single-flight 锁前缀。
const LOCK_PREFIX: &str = "bff:token_exchange_lock:";
/// 内存缓存后端默认 TTL 上限（`InMemoryCache::default` = 300s，§6.2）。
const BACKEND_MAX_TTL: Duration = Duration::from_secs(300);
/// `expires_in` 参与 TTL 计算时的安全余量（§6.2）。
const TTL_SKEW: Duration = Duration::from_secs(30);
/// single-flight 等待者重读缓存的最大轮数（每轮 sleep 100ms）。
const WAIT_ROUNDS: usize = 5;

/// 一次成功交换的产物。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenExchangeResult {
    pub access_token: String,
    pub expires_in: Option<u64>,
    pub scope: Option<String>,
}

/// 语义化交换错误（§3.4 错误表 → §7.1 分类）。
#[derive(Debug, thiserror::Error)]
pub enum ExchangeError {
    /// `invalid_grant` / `invalid_token` → 可重试（刷新会话后重试一次）
    #[error("subject token 无效或过期")]
    InvalidSubject,
    /// `access_denied` / `invalid_scope` → 不可重试
    #[error("交换被拒绝: {0}")]
    Denied(String),
    /// `invalid_client` / `invalid_request` / `unauthorized_client` → BFF 配置/实现错误
    #[error("客户端配置错误: {0}")]
    ClientConfig(String),
    /// `unsupported_grant_type` / 网络错误 / 5xx → token endpoint 故障
    #[error("token endpoint 故障: {0}")]
    Upstream(String),
}

impl ExchangeError {
    /// 分类计数（§8）。
    fn metric_label(&self) -> &'static str {
        match self {
            ExchangeError::InvalidSubject => "invalid_subject",
            ExchangeError::Denied(_) => "denied",
            ExchangeError::ClientConfig(_) => "client_config",
            ExchangeError::Upstream(_) => "upstream",
        }
    }

    fn into_app_error(self) -> AppError {
        match self {
            ExchangeError::InvalidSubject => AppError::unauthorized("令牌交换失败：会话令牌已失效"),
            ExchangeError::Denied(_) => AppError::unauthorized("令牌交换被拒绝"),
            ExchangeError::ClientConfig(_) => AppError::internal("令牌交换配置错误"),
            ExchangeError::Upstream(_) => AppError::bad_gateway("令牌交换服务暂不可用"),
        }
    }
}

/// 执行一次 RFC 8693 交换（不做缓存）。
///
/// - 客户端认证：`client_secret_basic`（默认）或 `client_secret_post`；
/// - `client_secret` 为空时按 public client 处理（不认证）；
/// - 响应仅接受 `Bearer` 且 `issued_token_type` 为合法 token-type URN。
pub async fn exchange(
    state: &AppState,
    cfg: &TokenExchangeConfig,
    token_endpoint: &str,
    subject_token: &str,
) -> Result<TokenExchangeResult, ExchangeError> {
    let mut form: Vec<(String, String)> = vec![
        (
            "grant_type".into(),
            "urn:ietf:params:oauth:grant-type:token-exchange".into(),
        ),
        ("subject_token".into(), subject_token.to_string()),
        ("subject_token_type".into(), cfg.subject_token_type.clone()),
    ];
    for a in &cfg.audience {
        form.push(("audience".into(), a.clone()));
    }
    if !cfg.scope.is_empty() {
        form.push(("scope".into(), cfg.scope.clone()));
    }
    form.push((
        "requested_token_type".into(),
        cfg.requested_token_type.clone(),
    ));
    // 委托场景（actor_token）为设计预留：本期无 actor_token 值来源，配置后不生效（§4.2），不发送
    if cfg.client_auth_method == TokenExchangeAuthMethod::ClientSecretPost {
        form.push(("client_id".into(), cfg.client_id.clone()));
        form.push(("client_secret".into(), cfg.client_secret.clone()));
    }

    let mut req = state.http.post(token_endpoint).form(&form);
    if cfg.client_auth_method == TokenExchangeAuthMethod::ClientSecretBasic
        && !cfg.client_secret.is_empty()
    {
        req = req.basic_auth(&cfg.client_id, Some(&cfg.client_secret));
    }

    let started = std::time::Instant::now();
    let resp = req
        .send()
        .await
        .map_err(|e| ExchangeError::Upstream(format!("token endpoint 请求失败: {}", e)))?;
    metrics::histogram!("bff_token_exchange_duration_seconds")
        .record(started.elapsed().as_secs_f64());

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    let err_code = body
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let err_desc = body
        .get("error_description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if !status.is_success() {
        let axum_status = axum::http::StatusCode::from_u16(status.as_u16())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        return classify_error(axum_status, &err_code, err_desc);
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExchangeError::ClientConfig("响应缺少 access_token".into()))?
        .to_string();
    let token_type = body
        .get("token_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !token_type.eq_ignore_ascii_case("bearer") {
        return Err(ExchangeError::ClientConfig(format!(
            "仅接受 Bearer token_type，收到: {}",
            token_type
        )));
    }
    let issued = body
        .get("issued_token_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !issued.is_empty() && !issued.starts_with("urn:ietf:params:oauth:token-type:") {
        return Err(ExchangeError::ClientConfig(format!(
            "issued_token_type 非法: {}",
            issued
        )));
    }
    let expires_in = body.get("expires_in").and_then(|v| v.as_u64());
    let scope = body
        .get("scope")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    metrics::counter!("bff_token_exchange_total", "result" => "exchange").increment(1);
    Ok(TokenExchangeResult {
        access_token,
        expires_in,
        scope,
    })
}

/// 将 401/400 响应解析为语义化错误（§3.4 表）。
fn classify_error(
    status: StatusCode,
    code: &str,
    desc: String,
) -> Result<TokenExchangeResult, ExchangeError> {
    metrics::counter!("bff_token_exchange_error_total", "error" => code.to_string()).increment(1);
    tracing::warn!(
        status = status.as_u16(),
        error = %code,
        error_description = %desc,
        "token exchange 失败"
    );
    match code {
        "invalid_grant" | "invalid_token" => Err(ExchangeError::InvalidSubject),
        "access_denied" | "invalid_scope" => Err(ExchangeError::Denied(desc)),
        "invalid_client" | "invalid_request" | "unauthorized_client" => {
            Err(ExchangeError::ClientConfig(desc))
        }
        "unsupported_grant_type" => Err(ExchangeError::Upstream(desc)),
        // 无标准错误码：按 HTTP 语义兜底
        _ if status.is_server_error() => Err(ExchangeError::Upstream(desc)),
        _ if status == StatusCode::UNAUTHORIZED => Err(ExchangeError::ClientConfig(desc)),
        _ => Err(ExchangeError::ClientConfig(desc)),
    }
}

// ── 缓存（§6） ──

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 缓存键 = `session_id + 配置指纹 + subject token 指纹`（§6.1）。
///
/// - 配置指纹：两条路由共享 token_endpoint 但 audience/scope 不同 → 键不同，防跨路由串用；
/// - subject 指纹：会话刷新后 access_token 变化 → 键自动失效，零耦合。
fn cache_key(session_id: &str, cfg: &TokenExchangeConfig, subject_token: &str) -> String {
    let cfg_fp = &sha256_hex(&cfg.fingerprint())[0..16];
    let sub_fp = &sha256_hex(subject_token)[0..16];
    format!("{}{}:{}:{}", CACHE_PREFIX, session_id, cfg_fp, sub_fp)
}

/// 有效 TTL = min(cache_ttl, expires_in - skew, backend_max_ttl)（§6.2）。
fn effective_ttl(cache_ttl: Duration, expires_in: Option<u64>) -> Duration {
    let mut ttl = cache_ttl;
    if let Some(exp) = expires_in {
        let capped = Duration::from_secs(exp).saturating_sub(TTL_SKEW);
        if capped < ttl {
            ttl = capped;
        }
    }
    if ttl > BACKEND_MAX_TTL {
        ttl = BACKEND_MAX_TTL;
    }
    if ttl <= Duration::ZERO {
        ttl = Duration::from_secs(1);
    }
    ttl
}

/// 面向代理的入口：缓存命中优先；miss 时 single-flight 交换并写缓存；
/// `invalid_grant`/`invalid_token` 时刷新会话 token 后重试**一次**（§7.2），仍失败 → 401。
pub async fn resolve(
    state: &AppState,
    session: &Session,
    cfg: &TokenExchangeConfig,
) -> Result<TokenExchangeResult, AppError> {
    let subject_token = current_access_token(session)
        .await
        .ok_or_else(|| AppError::unauthorized("未登录或会话已过期"))?;
    let session_id = session
        .id()
        .map(|id| id.to_string())
        .unwrap_or_else(|| "unknown".into());
    let key = cache_key(&session_id, cfg, &subject_token);

    // 1. 缓存命中
    if let Some(res) = read_cache(state, &key).await {
        metrics::counter!("bff_token_exchange_total", "result" => "cache_hit").increment(1);
        return Ok(res);
    }

    // 2. single-flight：持锁者执行交换并写缓存（§6.4）
    let lock_key = format!("{}{}", LOCK_PREFIX, key);
    if let Some(guard) = state
        .lock
        .acquire(
            &lock_key,
            Duration::from_millis(500),
            Duration::from_secs(5),
        )
        .await
    {
        // 持锁后重读缓存：可能已被并发请求写入
        if let Some(res) = read_cache(state, &key).await {
            metrics::counter!("bff_token_exchange_total", "result" => "cache_hit").increment(1);
            guard.release().await;
            return Ok(res);
        }
        let outcome = do_exchange_with_retry(state, session, cfg, &subject_token).await;
        store_result(state, &key, cfg.cache_ttl, &outcome).await;
        guard.release().await;
        return outcome.map_err(ExchangeError::into_app_error);
    }

    // 3. 未拿到锁：有界等待后重读缓存；仍 miss 则 fall-through 自行交换（§6.4）
    for _ in 0..WAIT_ROUNDS {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Some(res) = read_cache(state, &key).await {
            metrics::counter!("bff_token_exchange_total", "result" => "cache_hit").increment(1);
            return Ok(res);
        }
    }
    let outcome = do_exchange_with_retry(state, session, cfg, &subject_token).await;
    store_result(state, &key, cfg.cache_ttl, &outcome).await;
    outcome.map_err(ExchangeError::into_app_error)
}

/// 从缓存读取并反序列化。
async fn read_cache(state: &AppState, key: &str) -> Option<TokenExchangeResult> {
    let bytes = state.cache.get(key).await?;
    serde_json::from_slice::<TokenExchangeResult>(&bytes).ok()
}

/// 成功时按 effective TTL 写缓存；失败不缓存（§6.2）。
async fn store_result(
    state: &AppState,
    key: &str,
    cache_ttl: Duration,
    outcome: &Result<TokenExchangeResult, ExchangeError>,
) {
    match outcome {
        Ok(res) => {
            let ttl = effective_ttl(cache_ttl, res.expires_in);
            state
                .cache
                .set(key, serde_json::to_vec(res).unwrap_or_default(), ttl)
                .await;
        }
        Err(e) => {
            metrics::counter!("bff_token_exchange_total", "result" => "error").increment(1);
            metrics::counter!(
                "bff_token_exchange_error_total",
                "error" => e.metric_label().to_string()
            )
            .increment(1);
        }
    }
}

/// 执行交换；`invalid_grant`/`invalid_token` 时刷新会话 token 后重试一次（§7.2）。
async fn do_exchange_with_retry(
    state: &AppState,
    session: &Session,
    cfg: &TokenExchangeConfig,
    subject_token: &str,
) -> Result<TokenExchangeResult, ExchangeError> {
    let endpoint = resolve_token_endpoint(state, session, cfg).await?;
    match exchange(state, cfg, &endpoint, subject_token).await {
        Ok(res) => Ok(res),
        Err(ExchangeError::InvalidSubject) => {
            tracing::info!(
                route_token_exchange = true,
                "token exchange 收到 invalid_grant/invalid_token，刷新会话 token 后重试一次"
            );
            if let Some(tokens) = current_tokens(session).await {
                if force_refresh(state, session, &tokens).await.is_ok() {
                    if let Some(new_subject) = current_access_token(session).await {
                        // 刷新后 subject 变化 → 缓存键自动失效（§6.3），直接用新 subject 交换
                        return exchange(state, cfg, &endpoint, &new_subject).await;
                    }
                }
            }
            Err(ExchangeError::InvalidSubject)
        }
        Err(e) => Err(e),
    }
}

/// 解析 token endpoint：优先配置值，缺省回退会话 provider discovery（§4.4）。
async fn resolve_token_endpoint(
    state: &AppState,
    session: &Session,
    cfg: &TokenExchangeConfig,
) -> Result<String, ExchangeError> {
    if let Some(ep) = &cfg.token_endpoint {
        return Ok(ep.clone());
    }
    let tokens = current_tokens(session)
        .await
        .ok_or_else(|| ExchangeError::ClientConfig("无法解析会话 provider".into()))?;
    let cfg_snap = state.cfg();
    let provider = cfg_snap
        .oidc
        .providers
        .iter()
        .find(|p| p.id == tokens.provider)
        .ok_or_else(|| {
            ExchangeError::ClientConfig(format!("provider 不存在: {}", tokens.provider))
        })?;
    // openidconnect 的 CoreClient 不暴露 provider metadata，缺省路径做一次 discovery
    let issuer = openidconnect::IssuerUrl::new(provider.issuer_url.clone())
        .map_err(|e| ExchangeError::ClientConfig(format!("issuer_url 非法: {}", e)))?;
    let metadata = openidconnect::core::CoreProviderMetadata::discover_async(
        issuer,
        openidconnect::reqwest::async_http_client,
    )
    .await
    .map_err(|e| ExchangeError::ClientConfig(format!("OIDC discovery 失败: {}", e)))?;
    metadata
        .token_endpoint()
        .map(|u| u.to_string())
        .ok_or_else(|| {
            ExchangeError::ClientConfig("provider discovery 未提供 token endpoint".into())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_key_changes_with_config_and_subject() {
        let cfg_a = TokenExchangeConfig {
            client_id: "bff".into(),
            audience: vec!["admin-api".into()],
            scope: "admin.api".into(),
            ..Default::default()
        };
        let cfg_b = TokenExchangeConfig {
            client_id: "bff".into(),
            audience: vec!["other-api".into()],
            scope: "other.api".into(),
            ..Default::default()
        };
        let k1 = cache_key("sid", &cfg_a, "subject-1");
        let k2 = cache_key("sid", &cfg_b, "subject-1");
        let k3 = cache_key("sid", &cfg_a, "subject-2");
        let k4 = cache_key("sid", &cfg_a, "subject-1");
        assert_ne!(k1, k2, "audience/scope 不同应产生不同键");
        assert_ne!(k1, k3, "subject token 不同应产生不同键");
        assert_eq!(k1, k4);
        assert!(k1.starts_with(CACHE_PREFIX), "键应带前缀: {}", k1);
    }

    #[test]
    fn test_effective_ttl_caps() {
        // expires_in 兜底
        assert_eq!(
            effective_ttl(Duration::from_secs(300), Some(60)),
            Duration::from_secs(30)
        );
        // cache_ttl 生效
        assert_eq!(
            effective_ttl(Duration::from_secs(20), Some(3600)),
            Duration::from_secs(20)
        );
        // backend 上限 300s
        assert_eq!(
            effective_ttl(Duration::from_secs(600), None),
            Duration::from_secs(300)
        );
        // expires_in 过短 → 至少 1s
        assert_eq!(
            effective_ttl(Duration::from_secs(300), Some(10)),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn test_fingerprint_distinguishes_routes() {
        let a = TokenExchangeConfig {
            audience: vec!["admin-api".into()],
            scope: "admin.api".into(),
            ..Default::default()
        };
        let b = TokenExchangeConfig {
            audience: vec!["admin-api".into()],
            scope: "other.api".into(),
            ..Default::default()
        };
        assert_ne!(a.fingerprint(), b.fingerprint());
    }
}
