//! Task 18：迁移演练集成测试（§11.3 / §14-15 / §18-8）。
//!
//! 模拟两步发布的第 2 步（§11.3）：切换 `sites` 配置并把会话 cookie 名从
//! `BFF_SESSION` 轮换为 `BFF_SESSION_V2`、引入 Domain cookie（`cookie_domain`）后，
//! 旧部署签发的 host-only cookie 必须被新代码**忽略**——既不产生 5xx，也不得让
//! 持旧 cookie 的请求取得登录态；新状态重新登录后一切正常。
//!
//! 无状态依赖：旧/新状态为两份独立 `AppState`（各自内存会话存储），每次断言都用
//! 新建客户端并显式携带 cookie，重复执行结果稳定。

mod common;

/// 请求 `{base}/api/session`（可选显式 cookie），返回 JSON；
/// 非 2xx 直接 panic，因此同时也断言了“旧 cookie 不得触发 5xx”。
async fn session_state(base: &str, cookie: Option<&str>) -> serde_json::Value {
    let client = common::test_client();
    let mut req = client.get(format!("{base}/api/session"));
    if let Some(c) = cookie {
        req = req.header("cookie", c);
    }
    let resp = req.send().await.unwrap();
    assert!(
        resp.status().is_success(),
        "/api/session 应 2xx（含旧 cookie 不得 5xx），实际 {}",
        resp.status()
    );
    resp.json().await.unwrap()
}

/// §14-15：旧 host-only cookie 存在时，轮换 cookie 名后新会话不受影响。
///
/// 旧部署的 cookie 经**旧状态会话存储直接构造**（`common::login_cookie` /
/// `create_session_with_tokens`）而非重跑 OIDC 授权流程：这是产出该
/// `BFF_SESSION=<id>` host-only cookie 的最直接路径，也避免复制 `login_on` 中
/// 硬编码 `BFF_SESSION_V2` 的提取逻辑。
#[tokio::test]
async fn legacy_host_only_cookie_is_ignored_after_cookie_name_rotation() {
    // --- 旧部署：legacy 单站点 + host-only cookie BFF_SESSION ---
    let old_idp = common::spawn_mock_oidc_provider().await;
    let mut old_cfg = common::base_config();
    old_cfg
        .oidc
        .providers
        .push(common::mock_provider_cfg(&old_idp));
    assert_eq!(
        old_cfg.session.cookie_name, "BFF_SESSION",
        "legacy 默认 cookie 名应为 BFF_SESSION"
    );
    let old_state = common::make_state(old_cfg);
    let old_base = common::spawn_business(old_state.clone()).await;

    // 旧部署签发的 host-only cookie
    let old_cookie = common::login_cookie(&old_state).await;
    assert!(
        old_cookie.starts_with("BFF_SESSION="),
        "旧 cookie 应为 host-only BFF_SESSION=<id>，实际 {old_cookie}"
    );

    // 对照：旧代码接受该 cookie（证明它确为一个可用的旧会话，而非无效串）
    let body = session_state(&old_base, Some(&old_cookie)).await;
    assert_eq!(body["logged_in"], true, "旧状态应接受旧 cookie: {body}");

    // --- 新部署：两站点 + BFF_SESSION_V2 + Domain cookie ---
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let mut new_cfg = common::multisite_config(&idp_a, &idp_b);
    new_cfg.session.cookie_domain = Some(".example.com".into());
    let new_state = common::make_state(new_cfg);
    let app1 = common::spawn_site(&new_state, "app1").await;

    // 旧 cookie 在新站点被忽略：200（非 5xx）且 logged_in=false（旧会话不得泄漏）
    let body = session_state(&app1, Some(&old_cookie)).await;
    assert_eq!(body["logged_in"], false, "新站点不得沿用旧 cookie: {body}");
    assert!(body["provider"].is_null(), "旧 cookie 不应解析出 provider");

    // 重复执行（无状态依赖）：另起客户端仅带旧 cookie，结果稳定为未登录
    let repeat = session_state(&app1, Some(&old_cookie)).await;
    assert_eq!(repeat["logged_in"], false, "重复请求结果应稳定: {repeat}");

    // 新站点完整登录 → 正常认证
    let client = common::test_client();
    let cookie = common::login_on(&client, &idp_a, &app1, "pA", None).await;
    assert!(
        cookie.starts_with(&format!("{}=", common::SESSION_COOKIE)),
        "新会话 cookie 应为 {}，实际 {cookie}",
        common::SESSION_COOKIE
    );
    let body = session_state(&app1, Some(&cookie)).await;
    assert_eq!(body["logged_in"], true, "新状态重新登录应正常: {body}");
    assert_eq!(body["provider"], "pA");
}
