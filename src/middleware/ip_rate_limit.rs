//! 认证端点 per-IP 限流中间件（网络层纵深防御）。
//!
//! 与全局限流（tower-governor，默认 PeerIpKeyExtractor 按对端 IP 的 GCRA）互补：
//! - 按「来源 IP + 命中路径前缀」独立计数（令牌桶），复用 `CacheProvider` / `LockProvider`
//!   抽象，与 P1-6 cacheprovider 平滑切换保持一致；
//! - 支持配置可信代理数解析 `X-Forwarded-For`（`trusted_proxies = 0` 时不信任 XFF，
//!   防止伪造头绕过；LB 后配置为可信代理数，取 XFF 右侧第 N+1 项为客户端 IP）；
//! - 超限 → 429 + `Retry-After`，不进入上游，避免把打爆压力传导到 IAM；
//! - 可选审计日志（IP、路径、计数）；
//! - 未启用 / 未命中配置路径前缀的请求原样放行，不影响现有行为。
//!
//! 注意：本中间件只做 per-IP 维度限流，不做任何账号语义判断（账号锁定归 IAM）。
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// 令牌桶状态（cache 中序列化存储）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TokenBucket {
    /// 当前可用令牌数
    tokens: f64,
    /// 上次补液时刻（unix 秒）
    last: f64,
}

pub async fn ip_rate_limit_middleware(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let cfg = state.cfg();
    let rl = &cfg.auth_rate_limit;

    // 未启用 / 无路径 / 档位非法 → 放行（不影响现有行为）
    if !rl.enabled || rl.paths.is_empty() || rl.per_ip.per_second == 0 || rl.per_ip.burst_size == 0
    {
        return next.run(req).await;
    }

    let path = req.uri().path().to_string();
    // 命中配置的路径前缀（最长前缀优先，如 /oauth2/authorize 优先于 /oauth2）
    let Some(prefix) = rl
        .paths
        .iter()
        .filter(|p| path.starts_with(p.as_str()))
        .max_by_key(|p| p.len())
    else {
        return next.run(req).await;
    };

    let peer_ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    let Some(ip) = client_ip(&req, peer_ip, rl.trusted_proxies) else {
        // 无法确定来源 IP（理论不可达，服务器均带 ConnectInfo）：放行
        tracing::warn!(%path, "auth_rate_limit: 无法确定来源 IP，放行");
        return next.run(req).await;
    };

    let rate = rl.per_ip.per_second as f64;
    let capacity = rl.per_ip.burst_size as f64;
    let key = format!("bff:iprl:{}:{}", prefix, ip);

    // 同 key 读改写用 LockProvider 保证原子性（复用现有抽象）
    let lock_key = format!("bff:iprl:lock:{}:{}", prefix, ip);
    let Some(guard) = state
        .lock
        .acquire(
            &lock_key,
            Duration::from_millis(100),
            Duration::from_secs(2),
        )
        .await
    else {
        // 锁竞争失败 → 放行（限流器自身不成为新的故障点）
        metrics::counter!("bff_iprl_lock_contention_total").increment(1);
        return next.run(req).await;
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);

    let mut bucket = state
        .cache
        .get(&key)
        .await
        .and_then(|v| serde_json::from_slice::<TokenBucket>(&v).ok())
        .unwrap_or(TokenBucket {
            tokens: capacity,
            last: now,
        });

    // 按经过时间补液
    let elapsed = (now - bucket.last).max(0.0);
    bucket.tokens = (bucket.tokens + elapsed * rate).min(capacity);
    bucket.last = now;

    let blocked;
    let retry_after;
    if bucket.tokens >= 1.0 {
        bucket.tokens -= 1.0;
        blocked = false;
        retry_after = 0;
    } else {
        // 距下一个令牌的等待秒数（向上取整，至少 1s）
        blocked = true;
        retry_after = ((1.0 - bucket.tokens) / rate).ceil().max(1.0) as u64;
    }

    // 写回桶状态（TTL = 空桶补满所需时间 + 60s 冗余，空闲 key 自动过期）
    let ttl = Duration::from_secs_f64(capacity / rate + 60.0);
    if let Ok(v) = serde_json::to_vec(&bucket) {
        state.cache.set(&key, v, ttl).await;
    }
    // 决策完成后立即释放锁，避免持有到上游响应返回（同 IP 请求不被串行化）
    drop(guard);

    if blocked {
        metrics::counter!("bff_iprl_blocked_total", "prefix" => prefix.clone()).increment(1);
        if rl.log_over_limit {
            tracing::warn!(
                %ip,
                %path,
                %prefix,
                tokens = bucket.tokens,
                retry_after,
                "auth_rate_limit: 认证端点 per-IP 限流触发 429"
            );
        }
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("Retry-After", retry_after.to_string())],
            axum::Json(serde_json::json!({
                "error": "too_many_requests",
                "detail": "请求过于频繁，请稍后重试",
            })),
        )
            .into_response();
    }

    metrics::counter!("bff_iprl_allowed_total", "prefix" => prefix.clone()).increment(1);
    next.run(req).await
}

/// 解析客户端 IP：
/// - `trusted_proxies == 0`：不信任 `X-Forwarded-For`，直接使用对端 IP（防止伪造绕过）；
/// - `trusted_proxies > 0`：XFF 最右侧 N 个条目为可信代理，取左侧第 `len - N - 1` 项作为
///   客户端 IP；若 XFF 缺失或条目不足则回退对端 IP。
fn client_ip(req: &Request<Body>, peer: Option<IpAddr>, trusted_proxies: usize) -> Option<IpAddr> {
    if trusted_proxies == 0 {
        return peer;
    }
    if let Some(ips) = parse_xff(req) {
        if ips.len() > trusted_proxies {
            return ips.get(ips.len() - trusted_proxies - 1).copied();
        }
    }
    peer
}

/// 解析 `X-Forwarded-For`：取最后一个头值，按逗号拆分并过滤非法项。
fn parse_xff(req: &Request<Body>) -> Option<Vec<IpAddr>> {
    let v = req
        .headers()
        .get_all("x-forwarded-for")
        .iter()
        .last()?
        .to_str()
        .ok()?;
    let ips: Vec<IpAddr> = v
        .split(',')
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .collect();
    if ips.is_empty() {
        None
    } else {
        Some(ips)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;

    fn req_with_xff(xff: &str) -> Request<Body> {
        Request::builder()
            .uri("/login")
            .header("x-forwarded-for", xff)
            .body(Body::empty())
            .unwrap()
    }

    fn req_plain() -> Request<Body> {
        Request::builder()
            .uri("/login")
            .body(Body::empty())
            .unwrap()
    }

    #[test]
    fn trusted_zero_ignores_xff() {
        // trusted_proxies = 0 → 忽略 XFF，即使伪造头也回退对端 IP
        assert_eq!(
            client_ip(
                &req_with_xff("1.2.3.4, 10.0.0.5"),
                Some(IpAddr::from([127, 0, 0, 1])),
                0
            ),
            Some(IpAddr::from([127, 0, 0, 1]))
        );
    }

    #[test]
    fn trusted_one_uses_left_of_rightmost() {
        // LB 后：XFF = [client, lb]，trusted=1 → 取 client
        assert_eq!(
            client_ip(
                &req_with_xff("1.2.3.4, 10.0.0.5"),
                Some(IpAddr::from([127, 0, 0, 1])),
                1
            ),
            Some(IpAddr::from([1, 2, 3, 4]))
        );
    }

    #[test]
    fn trusted_one_insufficient_entries_falls_back() {
        // XFF 只有 1 条但 trusted=1 → 条目不足，回退对端 IP
        assert_eq!(
            client_ip(
                &req_with_xff("1.2.3.4"),
                Some(IpAddr::from([127, 0, 0, 1])),
                1
            ),
            Some(IpAddr::from([127, 0, 0, 1]))
        );
    }

    #[test]
    fn trusted_one_multiple_hops() {
        // XFF = [client, proxy1, lb]，trusted=1 → 取 proxy1 左侧的 client
        assert_eq!(
            client_ip(
                &req_with_xff("1.2.3.4, 10.0.0.6, 10.0.0.5"),
                Some(IpAddr::from([127, 0, 0, 1])),
                1
            ),
            Some(IpAddr::from([10, 0, 0, 6]))
        );
    }

    #[test]
    fn missing_xff_falls_back_to_peer() {
        assert_eq!(
            client_ip(&req_plain(), Some(IpAddr::from([127, 0, 0, 1])), 2),
            Some(IpAddr::from([127, 0, 0, 1]))
        );
    }
}
