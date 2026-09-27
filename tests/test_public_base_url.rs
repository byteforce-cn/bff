//! P0-2 回归测试：回调地址推导与 OIDC 客户端缓存键。
//!
//! 验收（审计 v2）：
//! - 并发触发（伪造 Host 的 login）后，`redirect_uri` 始终等于 `public_base_url` 推导值；
//! - 未配置 `public_base_url` 时，未受信任的 Host 被拒绝（不再静默拼 http://<evil>）；
//! - 后台刷新路径不再污染请求路径的 redirect_uri（client 缓存键含 base_url）。

mod common;

use common::{base_config, make_state, spawn_business, test_client};

fn auth_params(location: &str) -> std::collections::HashMap<String, String> {
    let url = url::Url::parse(location).unwrap();
    url.query_pairs().into_owned().collect()
}

/// 配置 public_base_url 后：无论 Host 是什么，redirect_uri 恒定。
#[tokio::test]
async fn login_redirect_uri_always_uses_public_base_url() {
    let idp = common::spawn_mock_oidc_provider().await;
    let mut cfg = base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    cfg.server.public_base_url = Some("https://bff.example.com".into());
    let bff = spawn_business(make_state(cfg)).await;

    // 连续用伪造 Host 触发 login（模拟匿名污染尝试）
    for host in ["evil.example.com", "another-attacker.test"] {
        let resp = test_client()
            .get(format!("{}/login", bff))
            .header("host", host)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_redirection(), "应重定向到 IdP");
        let location = resp.headers()["location"].to_str().unwrap().to_string();
        let params = auth_params(&location);
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some("https://bff.example.com/auth/callback"),
            "redirect_uri 必须恒为 public_base_url 推导值（Host={}）",
            host
        );
    }
}

/// 未配置 public_base_url：未受信任 Host 被拒绝；白名单 Host 正常。
#[tokio::test]
async fn login_rejects_untrusted_host_without_public_base_url() {
    let idp = common::spawn_mock_oidc_provider().await;
    let mut cfg = base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    cfg.server.trusted_hosts = vec!["bff.internal".into()];
    let bff = spawn_business(make_state(cfg)).await;

    let resp = test_client()
        .get(format!("{}/login", bff))
        .header("host", "evil.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400, "未受信任 Host 必须被拒绝");

    let resp = test_client()
        .get(format!("{}/login", bff))
        .header("host", "bff.internal")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_redirection(), "白名单 Host 应正常");
    let location = resp.headers()["location"].to_str().unwrap().to_string();
    let params = auth_params(&location);
    assert_eq!(
        params.get("redirect_uri").map(String::as_str),
        Some("http://bff.internal/auth/callback")
    );
}

/// OIDC 客户端缓存键必须包含 base_url；invalidate 清除所有变体。
#[tokio::test]
async fn oidc_client_cache_is_keyed_by_base_url() {
    let idp = common::spawn_mock_oidc_provider().await;
    let mut cfg = base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    let state = make_state(cfg);
    let provider = state.cfg().oidc.providers[0].clone();

    let a = state
        .oidc_clients
        .get(&provider, "http://a.example")
        .await
        .unwrap();
    let b = state
        .oidc_clients
        .get(&provider, "http://b.example")
        .await
        .unwrap();
    assert!(
        !std::sync::Arc::ptr_eq(&a, &b),
        "不同 base_url 不得共享同一 client（P0-2）"
    );

    // 相同 base_url → 命中缓存（指针相同）
    let a2 = state
        .oidc_clients
        .get(&provider, "http://a.example")
        .await
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &a2));

    // invalidate 应清掉所有 base 变体
    state.oidc_clients.invalidate(&provider.id).await;
    let a3 = state
        .oidc_clients
        .get(&provider, "http://a.example")
        .await
        .unwrap();
    assert!(!std::sync::Arc::ptr_eq(&a, &a3), "invalidate 应清除缓存");
}
