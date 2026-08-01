//! 场景 5/6：编排 DAG 并行聚合 + 脚本合并；超时与 fail_fast。
mod common;

use std::time::{Duration, Instant};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn pipeline_yaml(users_url: &str, orders_url: &str, timeout: &str) -> String {
    format!(
        r#"
strategy:
  timeout: {timeout}
  error_handling: fail_fast
steps:
  - id: fetch_user
    type: http_request
    config:
      url: "{users_url}/users/{{userId}}"
      method: GET
      timeout: 3s
      cache_ttl: 60s
  - id: fetch_orders
    type: http_request
    config:
      url: "{orders_url}/orders?userId={{userId}}"
      method: GET
  - id: merge
    type: script
    depends_on: [fetch_user, fetch_orders]
    config:
      script: |
        let user = inputs["fetch_user"].body;
        let orders = inputs["fetch_orders"].body;
        #{{ user: user, orders: orders }}
"#
    )
}

#[tokio::test]
async fn orchestration_parallel_aggregation_with_script() {
    let users = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"name": "Alice"})))
        .expect(1)
        .mount(&users)
        .await;

    let orders = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/orders"))
        .and(query_param("userId", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{"id": 1}])))
        .expect(2) // 未配置缓存，两次调用都打到下游
        .mount(&orders)
        .await;

    let mut cfg = common::base_config();
    cfg.pipelines.insert(
        "user-orders".into(),
        serde_yaml::from_str(&pipeline_yaml(&users.uri(), &orders.uri(), "10s")).unwrap(),
    );
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    let resp = client
        .get(format!("{}/pipeline/user-orders?userId=1", bff))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body, serde_json::json!({"user": {"name": "Alice"}, "orders": [{"id": 1}]}));

    // 第二次调用：fetch_user 命中缓存（仍 1 次），fetch_orders 再次调用（共 2 次）
    let resp = client
        .get(format!("{}/pipeline/user-orders?userId=1", bff))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    users.verify().await;
    orders.verify().await;
}

#[tokio::test]
async fn orchestration_timeout_fails_fast() {
    // 慢下游：3 秒延迟；pipeline 整体超时 500ms
    let slow = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true}))
                .set_delay(Duration::from_secs(3)),
        )
        .mount(&slow)
        .await;

    let yaml = format!(
        r#"
strategy:
  timeout: 500ms
  error_handling: fail_fast
steps:
  - id: slow_call
    type: http_request
    config:
      url: "{}/slow"
      method: GET
"#,
        slow.uri()
    );
    let mut cfg = common::base_config();
    cfg.pipelines
        .insert("slow".into(), serde_yaml::from_str(&yaml).unwrap());
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    let start = Instant::now();
    let resp = client
        .get(format!("{}/pipeline/slow", bff))
        .send()
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(resp.status(), 504, "应返回超时: {:?}", resp);
    assert!(elapsed < Duration::from_secs(2), "应快速失败，实际 {:?}", elapsed);
}

/// M1: Pipeline 内 script step 可以通过 params 读取 session/env 注入值。
#[tokio::test]
async fn script_step_reads_params_from_session_env() {
    let yaml = r#"
strategy:
  timeout: 10s
  error_handling: fail_fast
steps:
  - id: enrich
    type: script
    config:
      script: |
        let user_id = inputs["user_id"];
        let stage = inputs["stage"];
        #{ user: user_id, env: stage }
"#;
    let mut cfg = common::base_config();
    cfg.pipelines
        .insert("params-test".into(), serde_yaml::from_str(yaml).unwrap());
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    // 通过 query string 传入 params（模拟 session/env 映射后的结果）
    let resp = client
        .get(format!(
            "{}/pipeline/params-test?user_id=user-123&stage=staging",
            bff
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body,
        serde_json::json!({"user": "user-123", "env": "staging"})
    );
}

/// M1: 上游 step 输出 + params 均可被 script step 访问。
/// params 在顶层，step 输出嵌套在 step_id.body 下。
#[tokio::test]
async fn script_step_params_and_step_outputs_both_accessible() {
    // mock 下游返回 {"name": "Alice"}
    let svc = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/data"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"name": "Alice"})),
        )
        .mount(&svc)
        .await;

    let yaml = format!(
        r#"
strategy:
  timeout: 10s
  error_handling: fail_fast
steps:
  - id: fetch
    type: http_request
    config:
      url: "{}/data"
      method: GET
  - id: merge
    type: script
    depends_on: [fetch]
    config:
      script: |
        let name = inputs["fetch"].body.name;     // 来自 HTTP step 输出
        let uid = inputs["user_id"];               // 来自 params
        let env = inputs["stage"];                 // 来自 params
        #{{ user: uid, name: name, env: env }}
"#,
        svc.uri()
    );

    let mut cfg = common::base_config();
    cfg.pipelines
        .insert("both-test".into(), serde_yaml::from_str(&yaml).unwrap());
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    // params 中有 user_id 和 stage
    let resp = client
        .get(format!(
            "{}/pipeline/both-test?user_id=user-123&stage=staging",
            bff
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body,
        serde_json::json!({"user": "user-123", "name": "Alice", "env": "staging"})
    );
}

/// M1: 无 session/env 映射的 pipeline 行为不变（向后兼容）。
#[tokio::test]
async fn pipeline_without_params_behaves_same() {
    let yaml = r#"
strategy:
  timeout: 10s
  error_handling: fail_fast
steps:
  - id: echo
    type: script
    config:
      script: |
        #{ ok: true, count: 42 }
"#;
    let mut cfg = common::base_config();
    cfg.pipelines
        .insert("no-params".into(), serde_yaml::from_str(yaml).unwrap());
    let state = common::make_state(cfg);
    let bff = common::spawn_business(state).await;
    let client = common::test_client();

    let resp = client
        .get(format!("{}/pipeline/no-params", bff))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body, serde_json::json!({"ok": true, "count": 42}));
}
