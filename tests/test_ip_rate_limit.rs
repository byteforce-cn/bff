//! 认证端点 per-IP 限流集成测试（TDD）。
//!
//! 测试范围：
//! - 默认关闭时不影响现有行为（回归）
//! - 命中认证路径前缀 → per-IP 桶耗尽后 429 + Retry-After
//! - 未命中路径前缀不受影响
//! - 不同来源 IP 桶相互独立
//! - trusted_proxies = 0 时不信任 X-Forwarded-For（伪造头无法绕过）
//!
//! 说明：测试通过 HTTP 直接打到 BFF 业务端口，对端 IP 均为 127.0.0.1；
//! 模拟「LB 后不同客户端」时设置 `X-Forwarded-For: <client>, <lb>` 并配置
//! trusted_proxies = 1（右侧 1 个条目为可信代理 LB，取左侧 client 作为限流键）。

mod common;

use bff::config::AuthRateLimitConfig;
use common::{base_config, make_state, spawn_business, test_client};
use std::time::Duration;

/// 构造启用 per-IP 限流的配置：burst 个令牌，每秒 rate 个。
fn auth_rate_cfg(trusted_proxies: usize, per_second: u64, burst: u32) -> AuthRateLimitConfig {
    AuthRateLimitConfig {
        enabled: true,
        trusted_proxies,
        per_ip: bff::config::IpRateLimitBucket {
            per_second,
            burst_size: burst,
        },
        paths: vec!["/login".into()],
        log_over_limit: true,
    }
}

/// 从客户端 IP 视角发一次请求（XFF = 客户端, LB），返回状态码。
async fn request_with_client(base: &str, client: &str) -> u16 {
    let resp = test_client()
        .get(format!("{}/login", base))
        .header("x-forwarded-for", format!("{}, 10.0.0.5", client))
        .send()
        .await
        .expect("请求失败");
    resp.status().as_u16()
}

/// T1：默认（关闭）→ 认证端点不限制，维持现状。
#[tokio::test]
async fn test_auth_rate_limit_disabled_regression() {
    let mut cfg = base_config();
    cfg.auth_rate_limit = AuthRateLimitConfig::default(); // enabled = false
    let base = spawn_business(make_state(cfg)).await;

    for _ in 0..15 {
        let status = request_with_client(&base, "1.2.3.4").await;
        assert_ne!(status, 429, "默认关闭时不应触发 per-IP 限流");
    }
}

/// T2：命中认证路径 → 桶耗尽后 429 + Retry-After。
#[tokio::test]
async fn test_auth_rate_limit_block_with_retry_after() {
    let mut cfg = base_config();
    cfg.auth_rate_limit = auth_rate_cfg(1, 1, 3);
    let base = spawn_business(make_state(cfg)).await;

    // 前 3 个请求消耗 3 个令牌（pass），第 4、5 个被拒（429）
    let mut passed = 0;
    let mut blocked = 0;
    let mut retry_after: Option<u64> = None;
    for _ in 0..5 {
        let client = test_client();
        let resp = client
            .get(format!("{}/login", base))
            .header("x-forwarded-for", "1.2.3.4, 10.0.0.5")
            .send()
            .await
            .expect("请求失败");
        if resp.status().as_u16() == 429 {
            blocked += 1;
            if let Some(v) = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
            {
                retry_after = Some(v.parse().unwrap_or(0));
            }
        } else {
            passed += 1;
        }
    }

    assert_eq!(passed, 3, "burst=3 时应放行前 3 个请求");
    assert_eq!(blocked, 2, "桶耗尽后应返回 429");
    assert!(retry_after.is_some(), "429 应携带 Retry-After");
    assert!(retry_after.unwrap() >= 1, "Retry-After 应 >= 1s");
}

/// T3：未命中配置路径前缀的请求不受影响。
#[tokio::test]
async fn test_auth_rate_limit_other_paths_unaffected() {
    let mut cfg = base_config();
    cfg.auth_rate_limit = auth_rate_cfg(1, 1, 3);
    let base = spawn_business(make_state(cfg)).await;

    let client = test_client();
    for _ in 0..10 {
        let resp = client
            .get(format!("{}/live", base))
            .send()
            .await
            .expect("请求失败");
        assert_ne!(
            resp.status().as_u16(),
            429,
            "/live 未配置限流，不应触发 429"
        );
    }
}

/// T4：不同来源 IP 桶相互独立。
#[tokio::test]
async fn test_auth_rate_limit_per_ip_independent() {
    let mut cfg = base_config();
    cfg.auth_rate_limit = auth_rate_cfg(1, 1, 3);
    let base = spawn_business(make_state(cfg)).await;

    // A 消耗 3 个令牌
    for _ in 0..3 {
        let status = request_with_client(&base, "1.1.1.1").await;
        assert_ne!(status, 429);
    }
    // A 的第 4 个请求被拒
    assert_eq!(request_with_client(&base, "1.1.1.1").await, 429, "A 桶耗尽");
    // B 独立桶：仍可放行
    for _ in 0..3 {
        let status = request_with_client(&base, "2.2.2.2").await;
        assert_ne!(status, 429, "B 桶应与 A 相互独立");
    }
    // B 的第 4 个请求也被拒
    assert_eq!(request_with_client(&base, "2.2.2.2").await, 429, "B 桶耗尽");
}

/// T5：trusted_proxies = 0 时不信任 X-Forwarded-For，伪造头无法绕过。
#[tokio::test]
async fn test_auth_rate_limit_trusted_zero_ignores_xff() {
    let mut cfg = base_config();
    cfg.auth_rate_limit = auth_rate_cfg(0, 1, 3);
    let base = spawn_business(make_state(cfg)).await;

    // 每次伪造不同的 XFF 客户端 IP（真实对端均为 127.0.0.1）
    let spoofs = ["9.9.9.1", "9.9.9.2", "9.9.9.3", "9.9.9.4"];
    let mut blocked = 0;
    for spoof in spoofs {
        let resp = test_client()
            .get(format!("{}/login", base))
            .header("x-forwarded-for", format!("{}, 10.0.0.5", spoof))
            .send()
            .await
            .expect("请求失败");
        if resp.status().as_u16() == 429 {
            blocked += 1;
        }
    }
    assert_eq!(
        blocked, 1,
        "所有请求共享对端 IP 桶，第 4 个应被拒（伪造 XFF 无效）"
    );
}

/// T6：补液语义 —— 等待一个补液周期后桶恢复可放行。
#[tokio::test]
async fn test_auth_rate_limit_refill_after_wait() {
    let mut cfg = base_config();
    cfg.auth_rate_limit = auth_rate_cfg(1, 10, 5); // 每秒补 10 个令牌
    let base = spawn_business(make_state(cfg)).await;

    // 打满 5 个令牌
    for _ in 0..5 {
        let status = request_with_client(&base, "1.2.3.4").await;
        assert_ne!(status, 429);
    }
    // 第 6 个被拒
    assert_eq!(request_with_client(&base, "1.2.3.4").await, 429);

    // 等待 ~1s（rate=10/s → 补回 10 个令牌，超出容量 5）
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let status = request_with_client(&base, "1.2.3.4").await;
    assert_ne!(status, 429, "补液后应恢复放行");
}
