//! 场景 4：管理 API 配置导入 / 导出与热重载。
mod common;

use bff::config::AppConfig;

const SCRIPT_PIPELINE: &str = r#"
hello:
  strategy:
    timeout: 5s
    error_handling: fail_fast
  steps:
    - id: greet
      type: script
      config:
        script: '({ msg: "hi from pipeline" })'
"#;

#[tokio::test]
async fn config_export_import_and_hot_reload() {
    let idp = common::spawn_mock_oidc_provider().await;
    let mut cfg = common::base_config();
    // P0-3：指定可识别的真实主密钥，断言导出绝不泄露
    cfg.bff_secret.secret = "e2e-master-secret-777".into();
    cfg.bff_secret.salt = "e2e-master-salt-888".into();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    let state = common::make_state(cfg);
    let admin = common::spawn_admin(state.clone()).await;
    let cookie = common::login_cookie(&state).await;
    let bff = common::spawn_business(state).await;
    let client = common::test_client();
    let auth = || "test-admin-token";

    // 1. 导出：包含 OIDC provider，client_secret 已脱敏
    let resp = client
        .get(format!("{}/admin/api/config/export", admin))
        .header("x-admin-token", auth())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let yaml = resp.text().await.unwrap();
    assert!(yaml.contains("mock"), "导出应包含 provider: {}", yaml);
    assert!(yaml.contains("***"), "导出应脱敏: {}", yaml);
    assert!(!yaml.contains("bff-secret"), "导出不应包含真实密钥");
    // P0-3：主密钥（secret/salt）绝不能出现在导出结果中
    assert!(
        !yaml.contains("e2e-master-secret-777"),
        "导出不得泄露 bff_secret.secret: {}",
        yaml
    );
    assert!(
        !yaml.contains("e2e-master-salt-888"),
        "导出不得泄露 bff_secret.salt: {}",
        yaml
    );

    // 2. 修改导出内容：新增一个纯脚本 pipeline
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    let new_pipeline: serde_yaml::Value = serde_yaml::from_str(SCRIPT_PIPELINE).unwrap();
    doc["pipelines"] = new_pipeline;
    let new_yaml = serde_yaml::to_string(&doc).unwrap();

    // 3. 导入 → 200
    let resp = client
        .post(format!("{}/admin/api/config/import", admin))
        .header("x-admin-token", auth())
        .header("content-type", "application/yaml")
        .body(new_yaml)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "导入失败: {:?}", resp.text().await);

    // 4. 新 pipeline 立即生效
    let resp = client
        .get(format!("{}/pipeline/hello", bff))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["msg"], "hi from pipeline");

    // 5. P0-3：导出→回导回环后，原管理口令必须仍然有效（哨兵已回填真实值）
    let resp = client
        .get(format!("{}/admin/api/config/export", admin))
        .header("x-admin-token", auth())
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "回导后原管理口令失效——导出入侵回环破坏了密钥（P0-3）"
    );
}

/// P0-3：热导入 bff_secret 必须被显式拒绝（不得静默分裂配置与运行态）。
#[tokio::test]
async fn import_rejects_bff_secret_change() {
    let cfg = common::base_config();
    let state = common::make_state(cfg);
    let admin = common::spawn_admin(state).await;
    let client = common::test_client();

    let mut new_cfg = common::base_config();
    new_cfg.server.business_port = 8080;
    new_cfg.server.admin_port = 8443;
    new_cfg.bff_secret.secret = "another-secret-999".into();
    let mut doc = serde_yaml::to_value(&new_cfg).unwrap();
    // 模拟导出→编辑回导：管理口令用哨兵，bff_secret 是“新密钥”
    doc["admin"]["auth_token"] = serde_yaml::Value::String("***".into());
    let yaml = serde_yaml::to_string(&doc).unwrap();

    let resp = client
        .post(format!("{}/admin/api/config/import", admin))
        .header("x-admin-token", "test-admin-token")
        .header("content-type", "application/yaml")
        .body(yaml)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422, "bff_secret 热更新应被拒绝");
    let body: serde_json::Value = resp.json().await.unwrap();
    let err = body["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("bff_secret"),
        "错误信息应明确指出 bff_secret 不可热更新: {}",
        err
    );
}

#[tokio::test]
async fn import_rejects_invalid_yaml() {
    let cfg = common::base_config();
    let state = common::make_state(cfg);
    let admin = common::spawn_admin(state).await;
    let client = common::test_client();

    // 格式错误的 YAML
    let resp = client
        .post(format!("{}/admin/api/config/import", admin))
        .header("x-admin-token", "test-admin-token")
        .header("content-type", "application/yaml")
        .body("server: [not, a, map\n  broken: {")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("解析失败"));

    // 结构正确但 pipeline 有循环依赖
    let mut bad: AppConfig = common::base_config();
    bad.pipelines = serde_yaml::from_str(
        r#"
loop:
  steps:
    - id: a
      type: script
      depends_on: [b]
      config: { script: "1" }
    - id: b
      type: script
      depends_on: [a]
      config: { script: "2" }
"#,
    )
    .unwrap();
    let yaml = serde_yaml::to_string(&bad).unwrap();
    let resp = client
        .post(format!("{}/admin/api/config/import", admin))
        .header("x-admin-token", "test-admin-token")
        .header("content-type", "application/yaml")
        .body(yaml)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422);
}

/// F5：provider 连通性校验与真实删除端点。
#[tokio::test]
async fn provider_verify_and_delete() {
    let idp = common::spawn_mock_oidc_provider().await;
    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    let state = common::make_state(cfg);
    let admin = common::spawn_admin(state.clone()).await;
    let client = common::test_client();

    // 1. verify：对可达的 mock IdP 执行 discovery → ok=true
    let resp = client
        .post(format!("{}/admin/api/v1/oidc/providers/mock/verify", admin))
        .header("x-admin-token", "test-admin-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true, "可达 IdP 应校验通过: {}", body);
    assert!(
        body["token_endpoint"]
            .as_str()
            .unwrap_or("")
            .contains("/token"),
        "应返回 token_endpoint: {}",
        body
    );

    // 2. 不可达 provider → ok=false（HTTP 仍 200，供 UI 展示错误）
    let mut cfg2 = state.cfg().as_ref().clone();
    let mut bad = common::mock_provider_cfg(&idp);
    bad.id = "unreachable".into();
    bad.issuer_url = "http://127.0.0.1:1".into();
    cfg2.oidc.providers.push(bad);
    state.replace_config(cfg2).unwrap();
    let resp = client
        .post(format!(
            "{}/admin/api/v1/oidc/providers/unreachable/verify",
            admin
        ))
        .header("x-admin-token", "test-admin-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false, "不可达 IdP 应返回 ok=false: {}", body);

    // 3. DELETE：真实删除并热更新
    let resp = client
        .delete(format!("{}/admin/api/v1/oidc/providers/mock", admin))
        .header("x-admin-token", "test-admin-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(
        state.cfg().oidc.providers.iter().all(|p| p.id != "mock"),
        "provider 应已删除"
    );

    // 4. 重复删除 → 404
    let resp = client
        .delete(format!("{}/admin/api/v1/oidc/providers/mock", admin))
        .header("x-admin-token", "test-admin-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
