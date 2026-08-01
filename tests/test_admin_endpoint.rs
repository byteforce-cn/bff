//! 场景 3：管理端点 IP 白名单 + Token 认证。
mod common;

#[tokio::test]
async fn admin_ip_whitelist_allows_localhost() {
    let cfg = common::base_config();
    let state = common::make_state(cfg);
    let admin = common::spawn_admin(state).await;
    let client = common::test_client();

    // 白名单含 127.0.0.1 + 正确 token → 200
    let resp = client
        .get(format!("{}/admin/api/health", admin))
        .header("x-admin-token", "test-admin-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 无 token → 401
    let resp = client
        .get(format!("{}/admin/api/health", admin))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn admin_ip_whitelist_rejects_non_whitelisted() {
    let mut cfg = common::base_config();
    // 白名单不含 127.0.0.1：本机请求应被拒
    cfg.admin.ip_whitelist = vec!["10.0.0.0/8".into()];
    let state = common::make_state(cfg);
    let admin = common::spawn_admin(state).await;
    let client = common::test_client();

    let resp = client
        .get(format!("{}/admin/api/health", admin))
        .header("x-admin-token", "test-admin-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn admin_ui_is_served() {
    let cfg = common::base_config();
    let state = common::make_state(cfg);
    let admin = common::spawn_admin(state).await;
    let client = common::test_client();

    let resp = client
        .get(format!("{}/", admin))
        .header("x-admin-token", "test-admin-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("BFF 管理控制台"));
}

/// M2: eval 端点支持 session/env 注入。
#[tokio::test]
async fn eval_script_with_session_and_env() {
    let cfg = common::base_config();
    let state = common::make_state(cfg);
    // 注册测试脚本
    state
        .scripts
        .write()
        .await
        .insert("test.rhai".into(), "let sub = inputs[\"sub\"]; let env = inputs[\"APP_ENV\"]; #{ sub: sub, env: env }".into());
    let admin = common::spawn_admin(state).await;
    let client = common::test_client();

    let resp = client
        .post(format!("{}/admin/api/scripts/test.rhai/eval", admin))
        .header("x-admin-token", "test-admin-token")
        .header("Content-Type", "application/json")
        .body(serde_json::json!({
            "inputs": {},
            "session": {
                "sub": "user-123",
                "provider": "google",
                "access_token": "simulated-token"
            },
            "env": {
                "APP_ENV": "staging"
            }
        }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["result"],
        serde_json::json!({"sub": "user-123", "env": "staging"})
    );
    // debug 字段标记注入来源
    assert_eq!(body["debug"]["session_injected"], true);
    assert_eq!(body["debug"]["env_injected"], true);
}

/// M2: eval 端点 session/env 为可选字段，不传时行为不变。
#[tokio::test]
async fn eval_script_without_session_env_behaves_same() {
    let cfg = common::base_config();
    let state = common::make_state(cfg);
    state
        .scripts
        .write()
        .await
        .insert("simple.rhai".into(), "#{ ok: true }".into());
    let admin = common::spawn_admin(state).await;
    let client = common::test_client();

    let resp = client
        .post(format!("{}/admin/api/scripts/simple.rhai/eval", admin))
        .header("x-admin-token", "test-admin-token")
        .header("Content-Type", "application/json")
        .body(serde_json::json!({
            "inputs": {}
        }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["result"], serde_json::json!({"ok": true}));
    // 未传 session/env 时 debug 标记为 false
    assert_eq!(body["debug"]["session_injected"], false);
    assert_eq!(body["debug"]["env_injected"], false);
}

/// M3: pipeline test 端点 — 试运行 pipeline，返回 step 级详情。
#[tokio::test]
async fn pipeline_test_endpoint_returns_step_details() {
    let yaml = r#"
strategy:
  timeout: 10s
  error_handling: fail_fast
steps:
  - id: enrich
    type: script
    config:
      script: |
        let uid = inputs["user_id"];
        let stage = inputs["stage"];
        #{ user: uid, env: stage }
"#;
    let mut cfg = common::base_config();
    cfg.pipelines
        .insert("test-pipe".into(), serde_yaml::from_str(yaml).unwrap());
    let state = common::make_state(cfg);
    let admin = common::spawn_admin(state).await;
    let client = common::test_client();

    let resp = client
        .post(format!("{}/admin/api/pipelines/test-pipe/test", admin))
        .header("x-admin-token", "test-admin-token")
        .header("Content-Type", "application/json")
        .body(serde_json::json!({
            "params": {
                "user_id": "user-123"
            },
            "session": {
                "sub": "user-123",
                "provider": "google"
            },
            "env": {
                "stage": "staging"
            }
        }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    // 聚合结果
    assert_eq!(body["body"], serde_json::json!({"user": "user-123", "env": "staging"}));
    // step 级详情
    assert_eq!(body["steps"][0]["id"], "enrich");
    assert_eq!(body["steps"][0]["status"], 200);
    assert!(body["steps"][0]["duration_ms"].as_u64().is_some());
    // session/env 注入标记
    assert_eq!(body["session_injected"], true);
}

/// M3: dry_run 模式 — 跳过 HTTP，仍执行 script。
#[tokio::test]
async fn pipeline_test_dry_run_skips_http_executes_script() {
    let yaml = r#"
strategy:
  timeout: 10s
  error_handling: fail_fast
steps:
  - id: http_step
    type: http_request
    config:
      url: "http://should-not-be-called.example.com/api"
      method: GET
  - id: script_step
    type: script
    depends_on: [http_step]
    config:
      script: |
        let mock_body = inputs["http_step"].body;
        #{ dry_run_ok: true, mock: mock_body }
"#;
    let mut cfg = common::base_config();
    cfg.pipelines
        .insert("dry-run-pipe".into(), serde_yaml::from_str(yaml).unwrap());
    let state = common::make_state(cfg);
    let admin = common::spawn_admin(state).await;
    let client = common::test_client();

    let resp = client
        .post(format!("{}/admin/api/pipelines/dry-run-pipe/test", admin))
        .header("x-admin-token", "test-admin-token")
        .header("Content-Type", "application/json")
        .body(serde_json::json!({
            "dry_run": true
        }).to_string())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["body"]["dry_run_ok"], true);
    // HTTP step 应返回 dry_run 标记
    assert_eq!(body["steps"][0]["id"], "http_step");
    assert_eq!(body["steps"][0]["dry_run"], true);
}

/// M4: enable_test_endpoints=false 时 test/eval 端点返回 403。
#[tokio::test]
async fn test_endpoints_disabled_when_config_false() {
    let mut cfg = common::base_config();
    cfg.admin.enable_test_endpoints = false;
    let state = common::make_state(cfg);
    let admin = common::spawn_admin(state).await;
    let client = common::test_client();

    // eval 端点 → 403
    let resp = client
        .post(format!("{}/admin/api/scripts/test.rhai/eval", admin))
        .header("x-admin-token", "test-admin-token")
        .header("Content-Type", "application/json")
        .body(r#"{"inputs":{}}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // pipeline test 端点 → 403
    let resp = client
        .post(format!("{}/admin/api/pipelines/any/test", admin))
        .header("x-admin-token", "test-admin-token")
        .header("Content-Type", "application/json")
        .body(r#"{}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // 但 health 端点仍正常
    let resp = client
        .get(format!("{}/admin/api/health", admin))
        .header("x-admin-token", "test-admin-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}
