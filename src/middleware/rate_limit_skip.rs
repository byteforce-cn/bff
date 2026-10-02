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
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use governor::middleware::NoOpMiddleware;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tower::Service;
use tower_governor::governor::{Governor, GovernorConfig, GovernorConfigBuilder};
use tower_governor::key_extractor::KeyExtractor;
use tower_governor::GovernorError;

/// 按「真实客户端 IP」限流的 key extractor。
///
/// 原实现用 `PeerIpKeyExtractor`（按对端 IP）：LB 拓扑下全站共享一个桶，
/// 真实业务超 50rps 即对**所有用户** 429，限流器反而成为单点放大器。
/// 本提取器复用统一 IP 解析（信任 `X-Forwarded-For` 的右侧 N 跳）。
#[derive(Clone, Debug)]
pub struct ClientIpKeyExtractor {
    pub trusted_proxies: usize,
}

impl KeyExtractor for ClientIpKeyExtractor {
    type Key = IpAddr;

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        crate::middleware::client_ip::resolve_client_ip(req.headers(), peer, self.trusted_proxies)
            .ok_or(GovernorError::UnableToExtractKey)
    }
}

/// 全局限流跳过层的运行状态：共享的 governor 配置（内部复用同一个 `RateLimiter`）+ 跳过前缀。
#[derive(Clone)]
pub struct RateLimitSkipState {
    governor_conf: Arc<GovernorConfig<ClientIpKeyExtractor, NoOpMiddleware>>,
    skip_prefixes: Vec<String>,
}

/// 将配置语义「每秒 per_second 个令牌」换算为 tower-governor 的 `period`
/// （**每 period 补 1 个令牌**）。
///
/// ⚠️ tower-governor 0.4.x 的 `GovernorConfigBuilder::per_second(n)` 是
/// 「每 **n 秒**补 1 个令牌」（周期语义），**不是**「每秒 n 个」。若把配置值直接
/// 传入，限流会被收紧 n 倍（如默认 50/s 实际变成每 50 秒 1 个——burst 耗尽后
/// 正常流量被长时间 429；20xx-xx 压测实验实测）。此处显式换算周期 = 1s / n。
///
/// 边界：钳制到 `[1, 1e9]`——0 在启动期已被 `validate()` 拒绝，此处防御性取 1s；
/// 上限 1e9 保证周期不小于 1ns，避免浮点下溢成零周期导致 governor 构建失败。
fn governor_period(per_second: u64) -> Duration {
    let rps = per_second.clamp(1, 1_000_000_000);
    Duration::from_secs_f64(1.0 / rps as f64)
}

/// 限流错误的文本响应（内部仅用于构造 429/500）。
fn text_response(status: StatusCode, body: String) -> Response {
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = status;
    resp
}

/// 自定义限流错误响应：
/// - 429 保留库默认文案（兼容既有测试/日志习惯），同时补上 **`Retry-After`** 头
///   （RFC 6585：429 应携带退避时长；与认证端点限流响应保持一致）以及 `x-ratelimit-after`；
/// - 其余错误保持与库默认行为等价的响应（提取失败 = 500）。
fn governor_error_handler(err: GovernorError) -> Response {
    match err {
        GovernorError::TooManyRequests { wait_time, .. } => {
            let mut resp = text_response(
                StatusCode::TOO_MANY_REQUESTS,
                format!("Too Many Requests! Wait for {}s", wait_time),
            );
            if let Ok(v) = HeaderValue::from_str(&wait_time.to_string()) {
                resp.headers_mut().insert("retry-after", v.clone());
                resp.headers_mut().insert("x-ratelimit-after", v);
            }
            resp
        }
        GovernorError::UnableToExtractKey => text_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable To Extract Key!".to_string(),
        ),
        GovernorError::Other { code, msg, headers } => {
            let mut resp = text_response(code, msg.unwrap_or_else(|| "Other Error".to_string()));
            if let Some(h) = headers {
                *resp.headers_mut() = h;
            }
            resp
        }
    }
}

/// 构建全局限流跳过状态（注册为 `from_fn_with_state` 的 state）。
pub fn rate_limit_skip_state(
    per_second: u64,
    burst_size: u32,
    skip_prefixes: Vec<String>,
    trusted_proxies: usize,
) -> RateLimitSkipState {
    RateLimitSkipState {
        governor_conf: Arc::new(
            GovernorConfigBuilder::default()
                .period(governor_period(per_second))
                .burst_size(burst_size)
                .key_extractor(ClientIpKeyExtractor { trusted_proxies })
                .error_handler(governor_error_handler)
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

#[cfg(test)]
mod tests {
    use super::governor_period;
    use std::time::Duration;

    /// 回归：`per_second` 必须换算为「1s / N」的补液周期（而非 N 秒/个）。
    #[test]
    fn period_is_inverse_of_rps() {
        assert_eq!(governor_period(1), Duration::from_secs(1));
        assert_eq!(governor_period(50), Duration::from_millis(20));
        assert_eq!(governor_period(1000), Duration::from_millis(1));
        assert_eq!(governor_period(100_000), Duration::from_micros(10));
    }

    /// 边界：不产生零周期（0 已被 validate 拒绝，此处防御性收敛为 1s）。
    #[test]
    fn period_is_never_zero() {
        assert_eq!(governor_period(0), Duration::from_secs(1));
        assert_eq!(governor_period(u64::MAX), Duration::from_nanos(1));
    }
}
