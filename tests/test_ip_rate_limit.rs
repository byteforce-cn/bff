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
//! 模拟「LB 后不同客户端」时设置 `X-Forwarded-For: <client>`（已统一为
//! nginx `proxy_add_x_forwarded_for` 语义：每跳追加其接收到的对端地址，
//! 因此单层 LB 后 XFF 仅含 client；`trusted_proxies` 为可信跳数，
//! 解析时取“跳过右侧 N 个条目后的第一个条目”，左侧伪造条目自动被忽略）。

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

/// 从客户端 IP 视角发一次请求（XFF = 客户端，nginx 语义），返回状态码。
async fn request_with_client(base: &str, client: &str) -> u16 {
    let resp = test_client()
        .get(format!("{}/login", base))
        .header("x-forwarded-for", client.to_string())
        .send()
        .await
        .expect("请求失败");
    resp.status().as_u16()
}

/// 默认（关闭）→ 认证端点不限制，维持现状。
#[tokio::test]
async fn test_auth_rate_limit_disabled_regression() {
    let mut cfg = base_config();
    cfg.auth_rate_limit = AuthRateLimitConfig::default(); // enabled = false
    let base = spawn_business(make_state(cfg)).await;

    for _ in 0..15 {
        let status = request_with_client(&base, "1.2.3.4").await;
        // 未配置 OIDC provider → 确定返回 400；关键断言：不是 429（限流未启用）
        assert_eq!(
            status, 400,
            "默认关闭时不应触发 per-IP 限流（应到达业务逻辑返回 400）"
        );
    }
}

/// 命中认证路径 → 桶耗尽后 429 + Retry-After。
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
            .header("x-forwarded-for", "1.2.3.4")
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

/// 未命中配置路径前缀的请求不受影响。
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
        assert_eq!(
            resp.status().as_u16(),
            200,
            "/live 未配置限流，应正常放行返回 200"
        );
    }
}

/// 不同来源 IP 桶相互独立。
#[tokio::test]
async fn test_auth_rate_limit_per_ip_independent() {
    let mut cfg = base_config();
    cfg.auth_rate_limit = auth_rate_cfg(1, 1, 3);
    let base = spawn_business(make_state(cfg)).await;

    // A 消耗 3 个令牌
    for _ in 0..3 {
        let status = request_with_client(&base, "1.1.1.1").await;
        assert_eq!(status, 400, "A 前 3 个请求应放行至业务逻辑");
    }
    // A 的第 4 个请求被拒
    assert_eq!(request_with_client(&base, "1.1.1.1").await, 429, "A 桶耗尽");
    // B 独立桶：仍可放行
    for _ in 0..3 {
        let status = request_with_client(&base, "2.2.2.2").await;
        assert_eq!(status, 400, "B 桶应与 A 相互独立");
    }
    // B 的第 4 个请求也被拒
    assert_eq!(request_with_client(&base, "2.2.2.2").await, 429, "B 桶耗尽");
}

/// trusted_proxies = 0 时不信任 X-Forwarded-For，伪造头无法绕过。
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
            .header("x-forwarded-for", spoof)
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

/// 补液语义 —— 等待一个补液周期后桶恢复可放行。
///
/// 必须使用「慢补液」参数（rate=1/s）使桶在 5 个请求内确定性耗尽：
/// 单请求净消耗 = 1 − rate × d，只要单请求时延 d < ~0.17s，
/// 第 6 个请求必然被拒（不依赖 27ms/60ms 等具体时延假设）。
#[tokio::test]
async fn test_auth_rate_limit_refill_after_wait() {
    let mut cfg = base_config();
    cfg.auth_rate_limit = auth_rate_cfg(1, 1, 5); // 每秒补 1 个令牌（避免快速补液掩盖桶耗尽）
    let base = spawn_business(make_state(cfg)).await;

    // 打满 5 个令牌
    for _ in 0..5 {
        let status = request_with_client(&base, "1.2.3.4").await;
        assert_ne!(status, 429);
    }
    // 第 6 个被拒（净消耗 1 − 1 × d > 0，5 个请求后桶必然见底）
    assert_eq!(request_with_client(&base, "1.2.3.4").await, 429);

    // 等待 ~1.1s（rate=1/s → 补回 ≥1 个令牌）
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let status = request_with_client(&base, "1.2.3.4").await;
    assert_ne!(status, 429, "补液后应恢复放行");
}
