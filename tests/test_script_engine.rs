//! 场景 8：脚本引擎隔离与安全。
mod common;

use std::time::{Duration, Instant};

#[tokio::test]
async fn heavy_script_is_terminated_and_server_stays_responsive() {
    let mut cfg = common::base_config();
    cfg.pipelines.insert(
        "heavy".into(),
        serde_yaml::from_str(
            r#"
strategy:
  timeout: 10s
  error_handling: fail_fast
steps:
  - id: burn
    type: script
    config:
      script: |
        let mut i = 0;
        for x in 0..100_000_000 { i += 1; }
        i
"#,
        )
        .unwrap(),
    );
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    // 超限脚本被拒绝（操作数上限 / 时间上限）
    let start = Instant::now();
    let resp = client
        .get(format!("{}/pipeline/heavy", bff))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status() == 502 || resp.status() == 504,
        "超限脚本应失败: {}",
        resp.status()
    );
    assert!(start.elapsed() < Duration::from_secs(8), "脚本应被终止");

    // 主线程未被阻塞：/live 仍然快速响应
    let start = Instant::now();
    let resp = client.get(format!("{}/live", bff)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn dangerous_functions_are_unavailable() {
    let cfg = common::base_config();
    let state = common::make_state(cfg);
    let admin = common::spawn_admin(state).await;
    let client = common::test_client();

    // 未注册的危险函数（如文件读取）不可用
    let resp = client
        .post(format!("{}/admin/api/scripts/x/eval", admin))
        .header("x-admin-token", "test-admin-token")
        .json(&serde_json::json!({
            "script": "read_file(\"/etc/passwd\")",
            "inputs": {}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422);

    // eval 动态执行被禁用
    let resp = client
        .post(format!("{}/admin/api/scripts/x/eval", admin))
        .header("x-admin-token", "test-admin-token")
        .json(&serde_json::json!({
            "script": "eval(\"1 + 1\")",
            "inputs": {}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422);

    // 正常脚本可用
    let resp = client
        .post(format!("{}/admin/api/scripts/x/eval", admin))
        .header("x-admin-token", "test-admin-token")
        .json(&serde_json::json!({
            "script": "inputs[\"a\"] + 1",
            "inputs": {"a": 41}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"], 42);
}
