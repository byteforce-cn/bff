//! 场景：Admin-UI 模拟登录 — 管理员通过弹窗完成 OIDC 登录，BFF 生成 Session。
mod common;

// ============================================================
// 1. redirect 白名单校验
// ============================================================

#[test]
fn test_validate_redirect() {
    // 合法：同源绝对路径
    assert!(bff::oidc::handlers::validate_redirect("/"));
    assert!(bff::oidc::handlers::validate_redirect("/admin/sessions"));
    assert!(bff::oidc::handlers::validate_redirect("/api/users"));

    // 非法：双斜杠绕过
    assert!(!bff::oidc::handlers::validate_redirect("//evil.com"));
    assert!(!bff::oidc::handlers::validate_redirect("//evil.com/admin"));

    // 非法：完整 URL
    assert!(!bff::oidc::handlers::validate_redirect("http://evil.com"));
    assert!(!bff::oidc::handlers::validate_redirect("https://evil.com"));

    // 非法：相对路径（不以 / 开头）
    assert!(!bff::oidc::handlers::validate_redirect("evil.com"));
    assert!(!bff::oidc::handlers::validate_redirect("../admin"));
    assert!(!bff::oidc::handlers::validate_redirect("\\evil.com"));
    assert!(!bff::oidc::handlers::validate_redirect("\\\\evil.com"));

    // 合法：带 query string
    assert!(bff::oidc::handlers::validate_redirect("/admin/sessions?tab=1"));

    // 非法：空字符串
    assert!(!bff::oidc::handlers::validate_redirect(""));
}

// ============================================================
// 2. popup 模式集成测试
// ============================================================

#[tokio::test]
async fn simulate_login_popup_flow() {
    let idp = common::spawn_mock_oidc_provider().await;

    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    let resp = client
        .get(format!("{}/login?provider=mock&redirect=/dashboard&popup=true", bff))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "popup 登录应 3xx 重定向到 IdP"
    );
    let location = resp.headers()["location"].to_str().unwrap().to_string();
    assert!(
        location.starts_with(&format!("{}/authorize", idp.url)),
        "应重定向到 IdP authorize 端点"
    );

    let auth_url = url::Url::parse(&location).unwrap();
    let params: std::collections::HashMap<_, _> =
        auth_url.query_pairs().into_owned().collect();
    let state_param = params.get("state").expect("应含 state").clone();
    let nonce = params.get("nonce").expect("应含 nonce").clone();
    *idp.nonce.lock().unwrap() = Some(nonce);

    // Step 2: 模拟 IdP 回调
    // → popup 模式应返回 HTML 页面（200），而非 302
    let resp = client
        .get(format!(
            "{}/auth/callback?code=mock-code&state={}",
            bff, state_param
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200, "popup 模式应返回 200 HTML");

    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        ct.contains("text/html"),
        "应返回 HTML 内容类型，实际: {}",
        ct
    );

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("postMessage"),
        "HTML 应包含 postMessage 脚本"
    );
    assert!(
        body.contains("oidc-done"),
        "HTML 应发送 oidc-done 消息"
    );
    assert!(body.contains("window.close"), "HTML 应调用 window.close");
}

// ============================================================
// 3. redirect 白名单拒绝非法值
// ============================================================

#[tokio::test]
async fn login_rejects_invalid_redirect() {
    let idp = common::spawn_mock_oidc_provider().await;

    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    // 非法 redirect → 降级为 /
    let resp = client
        .get(format!(
            "{}/login?provider=mock&redirect=//evil.com&popup=true",
            bff
        ))
        .send()
        .await
        .unwrap();

    assert!(
        resp.status().is_redirection(),
        "即使非法 redirect，也应重定向到 IdP（redirect 降级）"
    );
    let location = resp.headers()["location"].to_str().unwrap().to_string();
    // 应仍能重定向到 IdP（redirect 参数不影响 IdP 跳转）
    assert!(
        location.starts_with(&format!("{}/authorize", idp.url)),
        "非法 redirect 时 IdP 跳转应正常"
    );
}

// ============================================================
// 4. 非 popup 模式仍正常 302
// ============================================================

#[tokio::test]
async fn non_popup_callback_still_redirects() {
    let idp = common::spawn_mock_oidc_provider().await;

    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    // 不带 popup=true 的登录
    let resp = client
        .get(format!("{}/login?provider=mock&redirect=/dashboard", bff))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_redirection());

    let location = resp.headers()["location"].to_str().unwrap().to_string();
    let auth_url = url::Url::parse(&location).unwrap();
    let params: std::collections::HashMap<_, _> =
        auth_url.query_pairs().into_owned().collect();
    let state_param = params.get("state").unwrap().clone();
    let nonce = params.get("nonce").unwrap().clone();
    *idp.nonce.lock().unwrap() = Some(nonce);

    // 回调 → 应 302 到 /dashboard（非 popup）
    let resp = client
        .get(format!(
            "{}/auth/callback?code=mock-code&state={}",
            bff, state_param
        ))
        .send()
        .await
        .unwrap();

    assert!(
        resp.status().is_redirection(),
        "非 popup 模式应 302 重定向"
    );
    let location = resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(
        location, "/dashboard",
        "应重定向到 /dashboard"
    );
}
