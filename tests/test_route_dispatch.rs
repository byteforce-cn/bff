//! 统一路由分发器（`server/route_dispatcher.rs`）覆盖补测（第六轮 P1）。
//!
//! 审计 §2 P1 热点：该文件覆盖率长期最低（11.87%），根因是分发路径
//! （Static / Pipeline / Script / Proxy）与输入/输出映射大量分支仅有部分路径被走过。
//! 本文件经**业务端口真实 HTTP 链路**覆盖：
//! - Static：响应体 / 自定义头（含非法头名容错）/ 自定义状态码 / 非法状态码；
//! - Pipeline：内联 / 引用注册表 / 引用缺失（404）/ 两者皆缺（400）；
//! - Script：内联 / 引用缺失（404）/ 两者皆缺（400）；
//! - Proxy：缺少 upstream（400）；
//! - 鉴权：auth_required=true 匿名 401 / 带会话放行；
//! - 输入映射：query / body / header / path（F9）/ session / 优先级覆盖；
//! - 输出映射：pick / rename / wrap / status_map（含 default 兜底）；
//! - 匹配：段边界（F2）、方法过滤、最长前缀优先。

mod common;

use bff::config::{InputMapping, OutputMapping, PipelineDef, RouteDef, RouteType, RouteTypeConfig};
use common::{base_config, login_cookie, make_state, spawn_business, test_client};
use serde_json::json;
use std::collections::HashMap;

/// 构造测试路由（默认 auth_required=false，空映射，无方法限制）。
fn route(path: &str, route_type: RouteType, config: RouteTypeConfig) -> RouteDef {
    RouteDef {
        path: path.into(),
        methods: vec![],
        description: String::new(),
        auth_required: false,
        route_type,
        config,
        input_mapping: InputMapping::default(),
        output_mapping: OutputMapping::default(),
    }
}

/// 最小可执行 pipeline：script step 输出固定 JSON（含 status 字段供 status_map 用）。
fn denied_pipeline() -> PipelineDef {
    serde_yaml::from_str(
        r#"
strategy:
  timeout: 5s
  error_handling: fail_fast
steps:
  - id: emit
    type: script
    config:
      script: |
        ({ status: "denied", message: "forbidden-zone", leak: "internal" })
"#,
    )
    .expect("pipeline 定义解析失败")
}

// ============================================================
// Static
// ============================================================

#[tokio::test]
async fn static_route_returns_body_status_and_headers() {
    let mut cfg = base_config();
    cfg.routes.push(route(
        "/api/dt/static",
        RouteType::Static,
        RouteTypeConfig {
            status: Some(201),
            body: Some(json!({"pong": true})),
            headers: Some(HashMap::from([
                ("x-route".to_string(), "static".to_string()),
                // 非法头名（含空格）应被跳过而非 500——容错分支断言
                ("bad header".to_string(), "v".to_string()),
            ])),
            ..Default::default()
        },
    ));
    let base = spawn_business(make_state(cfg)).await;

    let resp = test_client()
        .get(format!("{}/api/dt/static", base))
        .send()
        .await
        .expect("请求失败");
    assert_eq!(resp.status().as_u16(), 201, "自定义状态码应生效");
    assert_eq!(resp.headers().get("x-route").unwrap(), "static");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body, json!({"pong": true}));
}

#[tokio::test]
async fn static_route_invalid_status_returns_500() {
    let mut cfg = base_config();
    cfg.routes.push(route(
        "/api/dt/badstatus",
        RouteType::Static,
        RouteTypeConfig {
            // http crate 接受 100..=999，>999 才会触发 StatusCode::from_u16 失败
            status: Some(1000),
            ..Default::default()
        },
    ));
    let base = spawn_business(make_state(cfg)).await;

    let resp = test_client()
        .get(format!("{}/api/dt/badstatus", base))
        .send()
        .await
        .expect("请求失败");
    assert_eq!(
        resp.status().as_u16(),
        500,
        "非法状态码（>999）应显式报错而非静默放行"
    );
}

// ============================================================
// Pipeline
// ============================================================

#[tokio::test]
async fn pipeline_route_inline_applies_pick_rename_and_status_map() {
    let mut cfg = base_config();
    let mut r = route(
        "/api/dt/flow",
        RouteType::Pipeline,
        RouteTypeConfig {
            pipeline_inline: Some(denied_pipeline()),
            ..Default::default()
        },
    );
    r.output_mapping = OutputMapping {
        // 先 pick（按原名），再 rename（message → msg）
        pick: vec!["status".into(), "message".into()],
        rename: HashMap::from([("msg".to_string(), "message".to_string())]),
        status_map: HashMap::from([("denied".to_string(), 403u16)]),
        wrap: None,
    };
    cfg.routes.push(r);
    let base = spawn_business(make_state(cfg)).await;

    let resp = test_client()
        .get(format!("{}/api/dt/flow", base))
        .send()
        .await
        .expect("请求失败");
    assert_eq!(
        resp.status().as_u16(),
        403,
        "status_map 应按响应体 status 字段查表"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body,
        json!({"status": "denied", "msg": "forbidden-zone"}),
        "pick 应剔除 leak 字段，rename 应把 message 改为 msg"
    );
}

#[tokio::test]
async fn pipeline_route_status_map_default_fallback_and_wrap() {
    let mut cfg = base_config();
    // wrap 与 status_map default 兜底（响应体无 status 字段时命中 default）
    let mut r = route(
        "/api/dt/wrap",
        RouteType::Pipeline,
        RouteTypeConfig {
            pipeline_inline: Some(denied_pipeline()),
            ..Default::default()
        },
    );
    r.output_mapping = OutputMapping {
        wrap: Some("data".into()),
        status_map: HashMap::from([("default".to_string(), 418u16)]),
        ..Default::default()
    };
    cfg.routes.push(r);
    let base = spawn_business(make_state(cfg)).await;

    let resp = test_client()
        .get(format!("{}/api/dt/wrap", base))
        .send()
        .await
        .expect("请求失败");
    assert_eq!(
        resp.status().as_u16(),
        418,
        "status 不在表内时应回退 default"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["data"]["status"], "denied", "wrap 应包裹整个结果");
}

#[tokio::test]
async fn pipeline_route_reference_and_missing_cases() {
    let mut cfg = base_config();
    cfg.pipelines.insert("denied".into(), denied_pipeline());
    cfg.routes.push(route(
        "/api/dt/ref",
        RouteType::Pipeline,
        RouteTypeConfig {
            pipeline: Some("denied".into()),
            ..Default::default()
        },
    ));
    cfg.routes.push(route(
        "/api/dt/ref-missing",
        RouteType::Pipeline,
        RouteTypeConfig {
            pipeline: Some("nope".into()),
            ..Default::default()
        },
    ));
    cfg.routes.push(route(
        "/api/dt/ref-none",
        RouteType::Pipeline,
        RouteTypeConfig::default(),
    ));
    let base = spawn_business(make_state(cfg)).await;
    let client = test_client();

    let ok = client
        .get(format!("{}/api/dt/ref", base))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status().as_u16(), 200, "引用注册表 pipeline 应可执行");

    let missing = client
        .get(format!("{}/api/dt/ref-missing", base))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status().as_u16(), 404, "pipeline 引用缺失 → 404");

    let none = client
        .get(format!("{}/api/dt/ref-none", base))
        .send()
        .await
        .unwrap();
    assert_eq!(none.status().as_u16(), 400, "既无引用也无内联 → 400");
}

// ============================================================
// Script
// ============================================================

#[tokio::test]
async fn script_route_extracts_inputs_across_sources() {
    let mut cfg = base_config();
    let mut r = route(
        "/api/dt/users",
        RouteType::Script,
        RouteTypeConfig {
            script_inline: Some(
                r#"({ user: inputs["userId"], tag: inputs["tag"], from: inputs["who"] })"#.into(),
            ),
            ..Default::default()
        },
    );
    r.input_mapping = InputMapping {
        // F9：路径模板提取
        from_path: HashMap::from([(
            "userId".to_string(),
            "path./api/dt/users/{userId}".to_string(),
        )]),
        // query 优先级高于 body
        from_query: HashMap::from([("tag".to_string(), "tag".to_string())]),
        // session 扁平上下文（F8）
        from_session: HashMap::from([("who".to_string(), "sub".to_string())]),
        ..Default::default()
    };
    cfg.routes.push(r);
    let state = make_state(cfg);
    let cookie = login_cookie(&state).await;
    let base = spawn_business(state).await;

    let resp = test_client()
        .get(format!("{}/api/dt/users/42?tag=blue", base))
        .header("Cookie", cookie)
        .send()
        .await
        .expect("请求失败");
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["user"], "42", "F9：from_path 应从 URL 段提取");
    assert_eq!(body["tag"], "blue", "from_query 应提取 query 参数");
    assert_eq!(body["from"], "test-user", "from_session 应提取会话 sub");
}

#[tokio::test]
async fn script_route_extracts_from_env_with_prefixed_path() {
    // 第六轮修复回归：`env.NAME`（文档推荐形式）此前恒为 Null（被当 JSON 路径拆成
    // ["env","NAME"] 查询），修复后应按变量名取值；未引用的变量不得注入。
    std::env::set_var("BFF_DT_ENV_TOKEN", "from-env");
    let mut cfg = base_config();
    let mut r = route(
        "/api/dt/env",
        RouteType::Script,
        RouteTypeConfig {
            script_inline: Some(r#"({ tok: inputs["tok"], hasOther: "PATH" in inputs })"#.into()),
            ..Default::default()
        },
    );
    r.input_mapping = InputMapping {
        from_env: HashMap::from([("tok".to_string(), "env.BFF_DT_ENV_TOKEN".to_string())]),
        ..Default::default()
    };
    cfg.routes.push(r);
    let base = spawn_business(make_state(cfg)).await;

    let resp = test_client()
        .get(format!("{}/api/dt/env", base))
        .send()
        .await
        .expect("请求失败");
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["tok"], "from-env", "env.NAME 形式必须生效");
    assert_eq!(
        body["hasOther"], false,
        "S5：未显式引用的环境变量不得进入 inputs"
    );
}

#[tokio::test]
async fn script_route_priority_and_body_inputs() {
    // 优先级：defaults < body < query（同键时 query 覆盖 body）
    let mut cfg = base_config();
    let mut r = route(
        "/api/dt/pri",
        RouteType::Script,
        RouteTypeConfig {
            script_inline: Some(r#"({ v: inputs["v"] })"#.into()),
            ..Default::default()
        },
    );
    r.input_mapping = InputMapping {
        defaults: HashMap::from([("v".to_string(), json!("default"))]),
        from_body: HashMap::from([("v".to_string(), "v".to_string())]),
        from_query: HashMap::from([("v".to_string(), "v".to_string())]),
        ..Default::default()
    };
    cfg.routes.push(r);
    let base = spawn_business(make_state(cfg)).await;

    let resp = test_client()
        .post(format!("{}/api/dt/pri?v=from-query", base))
        .json(&json!({"v": "from-body"}))
        .send()
        .await
        .expect("请求失败");
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["v"], "from-query", "query 应覆盖 body 同名来源");
}

#[tokio::test]
async fn script_route_missing_and_none_cases() {
    let mut cfg = base_config();
    cfg.routes.push(route(
        "/api/dt/script-missing",
        RouteType::Script,
        RouteTypeConfig {
            script: Some("definitely-not-registered".into()),
            ..Default::default()
        },
    ));
    cfg.routes.push(route(
        "/api/dt/script-none",
        RouteType::Script,
        RouteTypeConfig::default(),
    ));
    let base = spawn_business(make_state(cfg)).await;
    let client = test_client();

    let missing = client
        .get(format!("{}/api/dt/script-missing", base))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status().as_u16(), 404, "脚本引用缺失 → 404");

    let none = client
        .get(format!("{}/api/dt/script-none", base))
        .send()
        .await
        .unwrap();
    assert_eq!(none.status().as_u16(), 400, "既无引用也无内联 → 400");
}

// ============================================================
// Proxy / 鉴权 / 匹配
// ============================================================

/// proxy 路由缺 upstream 在**配置校验期**即被拒绝（execute_proxy 的错误分支为纵深防御）。
#[test]
fn proxy_route_without_upstream_rejected_at_validation() {
    let mut cfg = base_config();
    // 与 make_state 一致：避开“端口相同”等无关校验的早退
    cfg.server.business_port = 8080;
    cfg.server.admin_port = 8443;
    cfg.routes.push(route(
        "/api/dt/proxy",
        RouteType::Proxy,
        RouteTypeConfig::default(),
    ));
    let err = cfg
        .validate()
        .expect_err("proxy 缺 upstream 应在校验期拒绝");
    assert!(
        err.to_string().contains("upstream"),
        "错误信息应指明 upstream: {}",
        err
    );
}

#[tokio::test]
async fn auth_required_route_rejects_anonymous_and_allows_session() {
    let mut cfg = base_config();
    let mut r = route(
        "/api/dt/private",
        RouteType::Static,
        RouteTypeConfig {
            status: Some(200),
            body: Some(json!({"secret": true})),
            ..Default::default()
        },
    );
    r.auth_required = true;
    cfg.routes.push(r);
    let state = make_state(cfg);
    let cookie = login_cookie(&state).await;
    let base = spawn_business(state).await;

    let anonymous = test_client()
        .get(format!("{}/api/dt/private", base))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status().as_u16(), 401, "匿名访问受保护路由 → 401");

    let authed = test_client()
        .get(format!("{}/api/dt/private", base))
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(authed.status().as_u16(), 200, "携带会话应放行");
}

#[tokio::test]
async fn route_matching_respects_segment_boundary_and_longest_prefix() {
    let mut cfg = base_config();
    cfg.routes.push(route(
        "/api/dt",
        RouteType::Static,
        RouteTypeConfig {
            body: Some(json!({"hit": "parent"})),
            ..Default::default()
        },
    ));
    cfg.routes.push(route(
        "/api/dt-secret",
        RouteType::Static,
        RouteTypeConfig {
            body: Some(json!({"hit": "secret"})),
            ..Default::default()
        },
    ));
    let base = spawn_business(make_state(cfg)).await;
    let client = test_client();

    // F2：/api/dt 不得命中 /api/dt-secret（段边界）
    let r1 = client
        .get(format!("{}/api/dt-secret", base))
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status().as_u16(), 200);
    assert_eq!(
        r1.json::<serde_json::Value>().await.unwrap()["hit"],
        "secret"
    );

    // 最长前缀优先：/api/dt-secret/x 命中更长的 /api/dt-secret 而非 /api/dt
    let r2 = client
        .get(format!("{}/api/dt-secret/x", base))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r2.json::<serde_json::Value>().await.unwrap()["hit"],
        "secret"
    );

    // 同级段内：/api/dt/x 命中 /api/dt
    let r3 = client
        .get(format!("{}/api/dt/x", base))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r3.json::<serde_json::Value>().await.unwrap()["hit"],
        "parent"
    );

    // 无边界匹配：/api/dt-secretx 不命中任一（/api 前缀 → 404 JSON）
    let r4 = client
        .get(format!("{}/api/dt-secretx", base))
        .send()
        .await
        .unwrap();
    assert_eq!(r4.status().as_u16(), 404);
}

#[tokio::test]
async fn route_method_filter_is_case_insensitive() {
    let mut cfg = base_config();
    let mut r = route(
        "/api/dt/method",
        RouteType::Static,
        RouteTypeConfig {
            body: Some(json!({"ok": true})),
            ..Default::default()
        },
    );
    r.methods = vec!["post".into()]; // 小写注册（大小写不敏感匹配）
    cfg.routes.push(r);
    let base = spawn_business(make_state(cfg)).await;
    let client = test_client();

    let get = client
        .get(format!("{}/api/dt/method", base))
        .send()
        .await
        .unwrap();
    assert_eq!(get.status().as_u16(), 404, "GET 不在 methods 列表 → 不匹配");

    let post = client
        .post(format!("{}/api/dt/method", base))
        .send()
        .await
        .unwrap();
    assert_eq!(post.status().as_u16(), 200, "POST 应匹配（大小写不敏感）");
}
