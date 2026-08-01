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
        script: '#{ msg: "hi from pipeline" }'
"#;

#[tokio::test]
async fn config_export_import_and_hot_reload() {
    let idp = common::spawn_mock_oidc_provider().await;
    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::mock_provider_cfg(&idp));
    let state = common::make_state(cfg);
    let admin = common::spawn_admin(state.clone()).await;
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

    // 2. 修改导出内容：新增一个纯脚本 pipeline
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    let new_pipeline: serde_yaml::Value =
        serde_yaml::from_str(SCRIPT_PIPELINE).unwrap();
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
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["msg"], "hi from pipeline");
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
