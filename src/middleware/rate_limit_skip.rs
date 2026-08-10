//! 全局限流路径跳过中间件（与 CSP `csp_overrides` 同风格，按路径前缀收窄）。
//!
//! 问题背景：tower-governor 全局限流挂在业务端口整个路由最外层，SPA 一次页面加载会并发请求
//! `index.html` + 大量 `/assets/*` 静态资源 + Monaco worker（轻松上百个 GET），全部计入同一个
//! 按对端 IP 共享的限流桶；多人/反复刷新即可瞬间打穿 burst → 429 + Retry-After。
//! 静态资源只有 IO/带宽成本，真正要保护的是 DB / 上游 API，因此把 SPA 资源从全局限流中摘除。
//!
//! 实现说明：不直接挂 `GovernorLayer`，而是用 `from_fn_with_state` 包一层——
//! - 命中 `skip_path_prefixes`：`next.run(req)` 直通，完全不进入限流器（不消耗令牌），
//!   且后续中间件（安全响应头 / 会话 / 指标等）照常生效；
//! - 其余路径：构造 `Governor::new(next, &config)` 执行原有限流逻辑。
//!   `Governor::new` 内部复用同一个 `Arc<RateLimiter>`，限流状态跨请求共享，与直接挂
//!   `GovernorLayer` 完全等价。
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use governor::middleware::NoOpMiddleware;
use std::sync::Arc;
use tower::Service;
use tower_governor::governor::{Governor, GovernorConfig, GovernorConfigBuilder};
use tower_governor::key_extractor::PeerIpKeyExtractor;

/// 全局限流跳过层的运行状态：共享的 governor 配置（内部复用同一个 `RateLimiter`）+ 跳过前缀。
#[derive(Clone)]
pub struct RateLimitSkipState {
    governor_conf: Arc<GovernorConfig<PeerIpKeyExtractor, NoOpMiddleware>>,
    skip_prefixes: Vec<String>,
}

/// 构建全局限流跳过状态（注册为 `from_fn_with_state` 的 state）。
pub fn rate_limit_skip_state(
    per_second: u64,
    burst_size: u32,
    skip_prefixes: Vec<String>,
) -> RateLimitSkipState {
    RateLimitSkipState {
        governor_conf: Arc::new(
            GovernorConfigBuilder::default()
                .per_second(per_second)
                .burst_size(burst_size)
                .finish()
                .expect("限流配置非法"),
        ),
        skip_prefixes,
    }
}

/// 全局限流中间件：命中 `skip_path_prefixes` 的请求不消耗全局限流令牌，其余路径保持限流。
pub async fn rate_limit_skip_middleware(
    State(state): State<RateLimitSkipState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    if state.skip_prefixes.iter().any(|p| path.starts_with(p)) {
        // 命中跳过前缀 → 不消耗全局限流令牌，直接进入后续中间件
        return next.run(request).await;
    }
    // 其余路径保持 tower-governor 全局限流（同一共享 limiter，状态跨请求一致）
    let mut governor = Governor::new(next.clone(), &state.governor_conf);
    match governor.call(request).await {
        Ok(resp) => resp,
        // Next 的 Error 为 Infallible，限流器的 429 已由 governor 内部转为响应
        Err(never) => match never {},
    }
}
