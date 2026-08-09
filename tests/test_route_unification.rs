//! 路由统一重构测试：RouteDef 序列化/反序列化
use bff::config::{AppConfig, InputMapping, OutputMapping, RouteType, RouteTypeConfig};
use serde_yaml;

// ============================================================
// RouteDef 反序列化测试
// ============================================================

#[test]
fn deserialize_proxy_route() {
    let yaml = r#"
routes:
  - path: "/api/mock"
    type: proxy
    description: "httpbin 测试代理"
    config:
      upstream: "http://httpbin.org"
      strip_prefix: true
      circuit_breaker_threshold: 5
"#;
    let cfg: AppConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.routes.len(), 1);
    let r = &cfg.routes[0];
    assert_eq!(r.path, "/api/mock");
    assert_eq!(r.route_type, RouteType::Proxy);
    assert_eq!(r.description, "httpbin 测试代理");
    assert_eq!(r.config.upstream.as_deref(), Some("http://httpbin.org"));
    assert_eq!(r.config.strip_prefix, true);
    assert_eq!(r.config.circuit_breaker_threshold, 5);
}

#[test]
fn deserialize_pipeline_route_referenced() {
    let yaml = r#"
routes:
  - path: "/api/dashboard"
    type: pipeline
    auth_required: true
    description: "用户仪表盘聚合"
    config:
      pipeline: "dashboard"
    input_mapping:
      from_query:
        userId: "userId"
      defaults:
        pageSize: 10
"#;
    let cfg: AppConfig = serde_yaml::from_str(yaml).unwrap();
    let r = &cfg.routes[0];
    assert_eq!(r.route_type, RouteType::Pipeline);
    assert_eq!(r.auth_required, true);
    assert_eq!(r.config.pipeline.as_deref(), Some("dashboard"));
    assert!(r.config.pipeline_inline.is_none());
    assert_eq!(r.input_mapping.from_query.get("userId").unwrap(), "userId");
    assert_eq!(
        r.input_mapping.defaults.get("pageSize").unwrap(),
        &serde_json::Value::Number(serde_json::Number::from(10))
    );
}

#[test]
fn deserialize_pipeline_route_inline() {
    let yaml = r#"
routes:
  - path: "/api/user-orders"
    type: pipeline
    auth_required: true
    config:
      pipeline_inline:
        strategy:
          timeout: "5s"
          error_handling: fail_fast
        steps:
          - id: fetch_user
            type: http_request
            config:
              url: "http://user-service/users/{userId}"
              method: GET
          - id: fetch_orders
            type: http_request
            config:
              url: "http://order-service/orders?userId={userId}"
              method: GET
    input_mapping:
      from_path:
        userId: "userId"
"#;
    let cfg: AppConfig = serde_yaml::from_str(yaml).unwrap();
    let r = &cfg.routes[0];
    assert_eq!(r.route_type, RouteType::Pipeline);
    assert!(r.config.pipeline.is_none());
    let inline = r.config.pipeline_inline.as_ref().unwrap();
    assert_eq!(inline.steps.len(), 2);
    assert_eq!(inline.steps[0].id, "fetch_user");
}

#[test]
fn deserialize_script_route() {
    let yaml = r#"
routes:
  - path: "/api/transform"
    type: script
    methods: ["POST"]
    description: "数据转换脚本"
    config:
      script: "transform.rhai"
    input_mapping:
      from_body:
        payload: "."
    output_mapping:
      wrap: "data"
"#;
    let cfg: AppConfig = serde_yaml::from_str(yaml).unwrap();
    let r = &cfg.routes[0];
    assert_eq!(r.route_type, RouteType::Script);
    assert_eq!(r.methods, vec!["POST"]);
    assert_eq!(r.config.script.as_deref(), Some("transform.rhai"));
    assert!(r.config.script_inline.is_none());
    assert_eq!(r.input_mapping.from_body.get("payload").unwrap(), ".");
    assert_eq!(r.output_mapping.wrap.as_deref(), Some("data"));
}

#[test]
fn deserialize_static_route() {
    let yaml = r#"
routes:
  - path: "/api/health-check"
    type: static
    config:
      status: 200
      body:
        status: "ok"
      headers:
        X-Custom: "bff"
"#;
    let cfg: AppConfig = serde_yaml::from_str(yaml).unwrap();
    let r = &cfg.routes[0];
    assert_eq!(r.route_type, RouteType::Static);
    assert_eq!(r.config.status, Some(200));
    assert_eq!(r.config.body.as_ref().unwrap()["status"], "ok");
    assert_eq!(
        r.config.headers.as_ref().unwrap().get("X-Custom").unwrap(),
        "bff"
    );
}

#[test]
fn deserialize_default_values() {
    let yaml = r#"
routes:
  - path: "/api/minimal"
    type: proxy
"#;
    let cfg: AppConfig = serde_yaml::from_str(yaml).unwrap();
    let r = &cfg.routes[0];
    assert_eq!(r.methods, Vec::<String>::new());
    assert_eq!(r.description, "");
    assert_eq!(r.auth_required, true); // default_true
    assert_eq!(r.config.strip_prefix, false); // default
    assert_eq!(r.config.circuit_breaker_threshold, 0); // default
}

#[test]
fn deserialize_multiple_routes() {
    let yaml = r#"
routes:
  - path: "/api/proxy1"
    type: proxy
    config:
      upstream: "http://svc1"
  - path: "/api/proxy2"
    type: proxy
    config:
      upstream: "http://svc2"
"#;
    let cfg: AppConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.routes.len(), 2);
    assert_eq!(cfg.routes[0].path, "/api/proxy1");
    assert_eq!(cfg.routes[1].path, "/api/proxy2");
}

// ============================================================
// RouteType 序列化
// ============================================================

#[test]
fn route_type_serde_roundtrip() {
    use serde_json;
    let types = vec![
        (RouteType::Proxy, "proxy"),
        (RouteType::Pipeline, "pipeline"),
        (RouteType::Script, "script"),
        (RouteType::Static, "static"),
    ];
    for (t, expected) in types {
        let s = serde_json::to_string(&t).unwrap();
        assert_eq!(s.trim_matches('"'), expected);
        let back: RouteType = serde_json::from_str(&s).unwrap();
        assert_eq!(back, t);
    }
}

// ============================================================
// InputMapping / OutputMapping 默认值
// ============================================================

#[test]
fn input_mapping_defaults() {
    let im: InputMapping = serde_json::from_str("{}").unwrap();
    assert!(im.from_query.is_empty());
    assert!(im.from_body.is_empty());
    assert!(im.from_path.is_empty());
    assert!(im.from_header.is_empty());
    assert!(im.defaults.is_empty());
}

#[test]
fn output_mapping_defaults() {
    let om: OutputMapping = serde_json::from_str("{}").unwrap();
    assert!(om.wrap.is_none());
    assert!(om.status_map.is_empty());
    assert!(om.rename.is_empty());
    assert!(om.pick.is_empty());
}

#[test]
fn route_type_config_default() {
    let rtc: RouteTypeConfig = serde_json::from_str("{}").unwrap();
    assert!(rtc.upstream.is_none());
    assert!(!rtc.strip_prefix);
    assert_eq!(rtc.circuit_breaker_threshold, 0);
    assert!(rtc.pipeline.is_none());
    assert!(rtc.pipeline_inline.is_none());
    assert!(rtc.script.is_none());
    assert!(rtc.script_inline.is_none());
    assert!(rtc.status.is_none());
    assert!(rtc.body.is_none());
    assert!(rtc.headers.is_none());
}
