//! 令牌刷新中间件：Stale-While-Revalidate 策略。
//!
//! - token 未临期：直接放行。
//! - token 临期（skew 窗口内，但仍有效）：用当前 token 放行，后台 spawn 异步刷新。
//! - token 已过期：阻塞等待 try_refresh 完成后再放行。
//!
//! 后台刷新使用 LockProvider 的 0ms try-lock 防止重复 spawn。
use crate::oidc::handlers::{current_tokens, try_refresh};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::State;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::Response;
use std::time::Duration;
use tower_sessions::Session;

pub async fn token_refresh_middleware(
    State(state): State<AppState>,
    session: Session,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    let skip_prefixes = &state.cfg().token_refresh.skip_prefixes;
    if skip_prefixes.iter().any(|p| path.starts_with(p.as_str())) {
        return next.run(req).await;
    }

    if let Some(tokens) = current_tokens(&session).await {
        let skew = state
            .cfg()
            .oidc
            .providers
            .iter()
            .find(|p| p.id == tokens.provider)
            .map(|p| p.refresh_skew_secs)
            .unwrap_or(60);

        let has_refresh_token = tokens.refresh_token_enc.is_some();

        if tokens.is_expiring(0) {
            // ============================================================
            // token 已过期：必须阻塞等待刷新
            // ============================================================
            match try_refresh(&state, &session, &tokens).await {
                Ok(Some(_)) => tracing::debug!("过期 access token 已刷新"),
                Ok(None) => tracing::debug!("过期 token 无可刷新令牌"),
                Err(e) => tracing::warn!(error = %e, "过期令牌刷新失败"),
            }
        } else if tokens.is_expiring(skew) && has_refresh_token {
            // ============================================================
            // token 临期但仍有效：Stale-While-Revalidate
            // try-lock(0ms) 防止重复 spawn 刷新任务
            // ============================================================
            let lock_key = format!(
                "bff:refresh_lock:{}",
                session
                    .id()
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "unknown".into())
            );
            let lock = state.lock.clone();
            let state = state.clone();
            let session = session.clone();
            let tokens = tokens.clone();

            tokio::spawn(async move {
                // try-lock(0ms) 仅作为防重复 spawn 的启发式检查
                let guard = lock
                    .acquire(&lock_key, Duration::from_millis(0), Duration::from_secs(10))
                    .await;
                if guard.is_none() {
                    // 已有其他任务在执行 try_refresh
                    return;
                }
                // 立即释放，让 try_refresh 自行管理锁
                guard.unwrap().release().await;

                match try_refresh(&state, &session, &tokens).await {
                    Ok(Some(_)) => tracing::debug!("后台 access token 刷新成功"),
                    Ok(None) => tracing::debug!("后台刷新无可刷新令牌"),
                    Err(e) => tracing::warn!(error = %e, "后台令牌刷新失败"),
                }
            });
        }
    }
    next.run(req).await
}
