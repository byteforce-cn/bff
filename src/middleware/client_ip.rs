//! S13：统一的客户端 IP 解析入口。
//!
//! 背景：仓库内曾有三套并存的 IP 解析逻辑（认证限流 XFF 模型、全局限流
//! `PeerIpKeyExtractor`、管理白名单 `ConnectInfo`），信任模型互不一致：
//! - LB 拓扑（浏览器 → LB → BFF）下，全局限流按对端 IP（= LB）建桶 → 全站共享一个桶；
//! - 管理白名单只看对端 IP → 经 LB 时语义失效；
//! - 认证限流的 XFF 取址有 off-by-one（`len - trusted - 1`）：
//!   nginx `proxy_add_x_forwarded_for` 追加的是**客户端**地址而非自身，
//!   正确取址应为 `len - trusted`（跳过右侧 `trusted_proxies` 个由可信代理追加的条目）。
//!
//! 本模块提供唯一实现，供三处复用。

use axum::http::HeaderMap;
use std::net::IpAddr;

/// 解析客户端真实 IP。
///
/// - `trusted_proxies == 0`：不信任 `X-Forwarded-For`，直接返回对端 IP（防伪造绕过）；
/// - `trusted_proxies > 0`：XFF 中跳过最右侧 `trusted_proxies` 个条目后取值；
///   条目不足时回退对端 IP。
///
/// 语义与 nginx `proxy_add_x_forwarded_for` 对齐：
/// 每跳把**其接收到的对端地址**追加到 XFF 右侧，因此跳过右侧 `trusted_proxies` 个
/// 条目后若仍有剩余，左侧第一个剩余条目即为真实客户端；条目不足则回退对端 IP。
pub fn resolve_client_ip(
    headers: &HeaderMap,
    peer: Option<IpAddr>,
    trusted_proxies: usize,
) -> Option<IpAddr> {
    if trusted_proxies == 0 {
        return peer;
    }
    if let Some(ips) = parse_xff(headers) {
        if ips.len() >= trusted_proxies {
            // 例：XFF=[client]，trusted=1 → idx=0 → client；
            //     XFF=[spoofed, client]，trusted=1 → idx=1 → client（左侧伪造被忽略）
            return ips.get(ips.len() - trusted_proxies).copied();
        }
    }
    peer
}

/// 解析 `X-Forwarded-For`：取最后一个头值，按逗号拆分并过滤非法项。
pub fn parse_xff(headers: &HeaderMap) -> Option<Vec<IpAddr>> {
    let v = headers
        .get_all("x-forwarded-for")
        .iter()
        .next_back()?
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

    fn headers_with_xff(xff: &str) -> HeaderMap {
        Request::builder()
            .uri("/")
            .header("x-forwarded-for", xff)
            .body(Body::empty())
            .unwrap()
            .headers()
            .clone()
    }

    fn peer() -> Option<IpAddr> {
        Some(IpAddr::from([192, 168, 1, 100]))
    }

    #[test]
    fn trusted_zero_ignores_xff() {
        assert_eq!(
            resolve_client_ip(&headers_with_xff("1.2.3.4"), peer(), 0),
            peer()
        );
    }

    #[test]
    fn nginx_single_entry_takes_client() {
        // nginx 只追加客户端（不含自身）：XFF=[client]，trusted=1 → client
        assert_eq!(
            resolve_client_ip(&headers_with_xff("1.2.3.4"), peer(), 1),
            Some(IpAddr::from([1, 2, 3, 4]))
        );
    }

    #[test]
    fn spoofed_left_entries_ignored() {
        // 客户端自带伪造 XFF，nginx 追加真实客户端 → XFF=[spoofed, client]
        // trusted=1 → 取最右侧 1 条（client），伪造被忽略
        assert_eq!(
            resolve_client_ip(&headers_with_xff("6.6.6.6, 1.2.3.4"), peer(), 1),
            Some(IpAddr::from([1, 2, 3, 4]))
        );
    }

    #[test]
    fn cdn_plus_lb_two_hops() {
        // XFF=[client, cdn]（LB 追加 CDN 地址），trusted=2 → client
        assert_eq!(
            resolve_client_ip(&headers_with_xff("1.2.3.4, 10.0.0.2"), peer(), 2),
            Some(IpAddr::from([1, 2, 3, 4]))
        );
    }

    #[test]
    fn insufficient_entries_falls_back_to_peer() {
        // XFF 条目少于可信跳数 → 无法确认客户端，回退对端 IP
        assert_eq!(
            resolve_client_ip(&headers_with_xff("1.2.3.4"), peer(), 2),
            peer()
        );
        assert_eq!(resolve_client_ip(&HeaderMap::new(), peer(), 1), peer());
    }
}
