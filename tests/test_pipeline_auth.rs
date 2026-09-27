//! P0-5 回归测试：`/pipeline/:name` 兼容入口必须要求认证。
//!
//! 背景（审计 v2 §P0-5）：该显式路由不经过统一路由分发器，
//! `auth_required` 对其不生效 → 匿名请求可触发任意 pipeline 真实执行。
//! 修复后：匿名 GET/POST 一律 401（且不暴露 pipeline 是否存在）。

mod common;

use bff::config::PipelineDef;
use common::{base_config, make_state, spawn_business, test_client};

/// 一个最小可执行 pipeline（script step）。
fn echo_pipeline() -> PipelineDef {
    serde_yaml::from_str(
        r#"
strategy:
  timeout: 5s
  error_handling: fail_fast
steps:
  - id: greet
    type: script
    config:
      script: '({ message: "hello from pipeline" })'
"#,
    )
    .expect("pipeline 定义解析失败")
}

/// 匿名 GET /pipeline/:name → 401（不得执行）。
#[tokio::test]
async fn anonymous_pipeline_get_returns_401() {
    let mut cfg = base_config();
    cfg.pipelines.insert("echo".into(), echo_pipeline());
    let base = spawn_business(make_state(cfg)).await;

    let resp = test_client()
        .get(format!("{}/pipeline/echo", base))
        .send()
        .await
        .expect("请求失败");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "匿名 GET /pipeline/:name 必须返回 401（P0-5）"
    );
}

/// 匿名 POST /pipeline/:name → 401（不得执行）。
#[tokio::test]
async fn anonymous_pipeline_post_returns_401() {
    let mut cfg = base_config();
    cfg.pipelines.insert("echo".into(), echo_pipeline());
    let base = spawn_business(make_state(cfg)).await;

    let resp = test_client()
        .post(format!("{}/pipeline/echo?foo=bar", base))
        .json(&serde_json::json!({"attacker_controlled": true}))
        .send()
        .await
        .expect("请求失败");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "匿名 POST /pipeline/:name 必须返回 401（P0-5）"
    );
}

/// 匿名访问不存在的 pipeline 同样 401（鉴权先于存在性检查，不泄露清单）。
#[tokio::test]
async fn anonymous_pipeline_not_found_still_401() {
    let cfg = base_config();
    let base = spawn_business(make_state(cfg)).await;

    let resp = test_client()
        .get(format!("{}/pipeline/does-not-exist", base))
        .send()
        .await
        .expect("请求失败");
    assert_eq!(
        resp.status().as_u16(),
        401,
        "鉴权应最先执行，避免匿名探测 pipeline 存在性"
    );
}
