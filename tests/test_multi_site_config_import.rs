//! Task 7 场景：多站点配置导入的结构门禁（§5.6 import 行为）。
//!
//! Task 10 前测试不可用 `common::multisite_config`——本地构造双站点配置（dev 语义：
//! 无 public_base_url，loopback 兜底；fake issuer URL 不发起真实网络请求）。

mod common;

use bff::config::AppConfig;

/// 本地双站点配置：每个站点独立 provider（callback_path 不同），共享 default profile
/// 的 Domain cookie（`.test`，dev 下显式 allow_unmanaged_subdomains 消除告警）。
fn multisite_cfg() -> AppConfig {
    serde_yaml::from_str(
        r#"
server:
  admin_port: 8443
admin:
  ip_whitelist: ["127.0.0.1"]
  auth_mode: "token"
  auth_token: "test-admin-token"
session:
  cookie_domain: ".test"
  allow_unmanaged_subdomains: true
sites:
  - name: app1
    port: 8081
    bind: "127.0.0.1"
    server_names: ["app1.test"]
    session_profile: default
    oidc:
      default_provider: idp1
  - name: app2
    port: 8083
    bind: "127.0.0.1"
    server_names: ["app2.test"]
    session_profile: default
    oidc:
      default_provider: idp2
oidc:
  providers:
    - id: idp1
      issuer_url: "http://idp1.test"
      client_id: "app1-client"
      callback_path: "/auth/callback-app1"
    - id: idp2
      issuer_url: "http://idp2.test"
      client_id: "app2-client"
      callback_path: "/auth/callback-app2"
"#,
    )
    .expect("测试配置解析失败")
}

/// 导入改变 `sites[0].port`（结构变更）→ 200 requires_restart，旧配置不变。
#[tokio::test]
async fn import_returns_requires_restart_and_keeps_old_config() {
    let state = common::make_state(multisite_cfg());
    let admin = common::spawn_admin(state.clone()).await;
    let client = common::test_client();

    // 模拟导出→修改→回导（哨兵回填路径与真实管理台一致）
    let mut next = state.cfg().sanitized();
    next.sites[0].port = 8090;
    let yaml = serde_yaml::to_string(&next).unwrap();

    let resp = client
        .post(format!("{}/admin/api/v1/config/import", admin))
        .header("x-admin-token", "test-admin-token")
        .header("content-type", "application/yaml")
        .body(yaml)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "requires_restart", "{}", body);
    assert!(
        body["requires_restart"]
            .as_array()
            .expect("requires_restart 应为数组")
            .iter()
            .any(|v| v.as_str() == Some("sites[app1].port")),
        "{}",
        body
    );
    assert_eq!(state.cfg().sites[0].port, 8081, "旧配置必须保持");
}

/// 导入删除 app2（站点删除属结构变更）→ requires_restart 含 sites[app2]，旧配置两份站点仍在。
#[tokio::test]
async fn import_site_removal_requires_restart() {
    let state = common::make_state(multisite_cfg());
    let admin = common::spawn_admin(state.clone()).await;
    let client = common::test_client();

    let mut next = state.cfg().sanitized();
    next.sites.remove(1);
    let yaml = serde_yaml::to_string(&next).unwrap();

    let resp = client
        .post(format!("{}/admin/api/v1/config/import", admin))
        .header("x-admin-token", "test-admin-token")
        .header("content-type", "application/yaml")
        .body(yaml)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "requires_restart", "{}", body);
    assert!(
        body["requires_restart"]
            .as_array()
            .expect("requires_restart 应为数组")
            .iter()
            .any(|v| v.as_str() == Some("sites[app2]")),
        "{}",
        body
    );
    assert_eq!(state.cfg().sites.len(), 2, "旧配置两份站点仍在");
}

/// 纯热变更（新增 pipeline）→ 200 applied，hot_applied 报 `pipelines` 组并立即生效。
#[tokio::test]
async fn import_hot_change_applies_and_reports_pipelines() {
    let state = common::make_state(multisite_cfg());
    let admin = common::spawn_admin(state.clone()).await;
    let client = common::test_client();

    let mut next = state.cfg().sanitized();
    next.pipelines.insert(
        "hello".into(),
        serde_yaml::from_str(
            r#"
strategy: {}
steps:
  - id: greet
    type: script
    config:
      script: '({ msg: "hi" })'
"#,
        )
        .unwrap(),
    );
    let yaml = serde_yaml::to_string(&next).unwrap();

    let resp = client
        .post(format!("{}/admin/api/v1/config/import", admin))
        .header("x-admin-token", "test-admin-token")
        .header("content-type", "application/yaml")
        .body(yaml)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "applied", "{}", body);
    let hot = body["hot_applied"]
        .as_array()
        .expect("hot_applied 应为数组");
    assert!(
        hot.iter().any(|v| v.as_str() == Some("pipelines")),
        "{}",
        body
    );
    assert!(
        state.cfg().pipelines.contains_key("hello"),
        "pipeline 应已生效"
    );
}
