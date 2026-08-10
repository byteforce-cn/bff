//! 场景 2：SPA 静态文件服务及 fallback。
mod common;

#[tokio::test]
async fn spa_serving_and_fallback() {
    let mut cfg = common::base_config();
    cfg.spa.dir = common::make_spa_dir("spa");
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    // 1. /index.html → 200 text/html
    let resp = client
        .get(format!("{}/index.html", bff))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
    assert!(ct.contains("text/html"), "content-type: {}", ct);
    assert!(resp.text().await.unwrap().contains("spa"));

    // 2. 静态资源 /app.js → 200
    let resp = client.get(format!("{}/app.js", bff)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("console.log"));

    // 3. 前端路由 fallback → index.html
    let resp = client
        .get(format!("{}/dashboard/settings", bff))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
    assert!(ct.contains("text/html"), "fallback content-type: {}", ct);
    assert!(resp.text().await.unwrap().contains("spa"));

    // 4. /api 下未命中路由 → 404（不回退到 index.html）
    let resp = client
        .get(format!("{}/api/nonexistent", bff))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// 按路径前缀细分的 CSP：编辑器路径放行 unsafe-eval，其余路径保持严格 script-src 'self'。
#[tokio::test]
async fn spa_csp_path_override() {
    use bff::config::CspOverrideConfig;

    let mut cfg = common::base_config();
    cfg.spa.dir = common::make_spa_dir("spa");
    cfg.security_headers.csp_overrides = vec![CspOverrideConfig {
        path_prefix: "/admin/templates/design".into(),
        content_security_policy:
            "default-src 'self'; script-src 'self' 'unsafe-eval'; style-src 'self' 'unsafe-inline'"
                .into(),
    }];
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    // 1. 编辑器路径（SPA fallback 命中 index.html）→ 放宽 CSP（含 unsafe-eval）
    let resp = client
        .get(format!("{}/admin/templates/design/123", bff))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let csp = resp.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(csp.contains("'unsafe-eval'"), "editor csp: {}", csp);

    // 2. 其余路径 → 严格 CSP（无 unsafe-eval）
    let resp = client
        .get(format!("{}/dashboard/settings", bff))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let csp = resp.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(!csp.contains("'unsafe-eval'"), "strict csp: {}", csp);

    // 3. 未配置 csp_overrides 时全局 CSP 默认严格（回归：不配置即不放宽）
    let mut cfg = common::base_config();
    cfg.spa.dir = common::make_spa_dir("spa");
    let bff = common::spawn_business(common::make_state(cfg)).await;
    let resp = client
        .get(format!("{}/admin/templates/design/123", bff))
        .send()
        .await
        .unwrap();
    let csp = resp.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(!csp.contains("'unsafe-eval'"), "default csp: {}", csp);
}

/// 全局限流路径跳过（与 CSP csp_overrides 同风格收窄）：
/// SPA 静态资源（skip_path_prefixes 命中）不消耗全局限流令牌，其余路径保持 tower-governor 限流。
#[tokio::test]
async fn rate_limit_skip_paths_bypass_global() {
    let mut cfg = common::base_config();
    cfg.spa.dir = common::make_spa_dir("spa");
    // 收紧全局限流，便于在测试中快速触达 429（默认 50/s、burst 500 需要大量请求）
    cfg.rate_limit.per_second = 1;
    cfg.rate_limit.burst_size = 2;
    // 与 CSP csp_overrides 同风格：SPA 静态资源路径跳过全局限流
    cfg.rate_limit.skip_path_prefixes = vec!["/index.html".into(), "/app.js".into()];
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    // 1. skip 路径（SPA 静态资源）：连续请求全部 200，不消耗令牌、不触发 429
    for i in 0..12 {
        let resp = client
            .get(format!("{}/app.js?t={}", bff, i))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "skip 路径不应被全局限流 (i={})", i);
    }

    // 2. 非 skip 路径（/api/*）：burst=2 用尽后应触发 429（限流器对非跳过路径正常工作）
    let mut saw_429 = false;
    for _ in 0..8 {
        let resp = client
            .get(format!("{}/api/nonexistent", bff))
            .send()
            .await
            .unwrap();
        if resp.status().as_u16() == 429 {
            saw_429 = true;
            let body = resp.text().await.unwrap();
            assert!(
                body.contains("Too Many Requests"),
                "429 响应应为 tower-governor 默认文案: {}",
                body
            );
            break;
        }
    }
    assert!(saw_429, "非 skip 路径应触发全局限流 429");
}
