//! 输入/输出映射引擎测试
use bff::config::{InputMapping, OutputMapping};
use serde_json::json;

// ============================================================
// InputMapping 测试
// ============================================================
// 注意：完整路径提取需要 axum Request 上下文，
// 这里测试纯逻辑：合并 defaults、JSON Path 提取等。

#[test]
fn merge_defaults_applied_when_no_input() {
    let im = InputMapping {
        defaults: {
            let mut m = std::collections::HashMap::new();
            m.insert("pageSize".into(), json!(20));
            m.insert("sort".into(), json!("desc"));
            m
        },
        ..Default::default()
    };
    let result = bff::server::mapping::merge_inputs(&im, &json!({}), &json!({}), &json!({}), &json!({}), &json!({}));
    assert_eq!(result["pageSize"], json!(20));
    assert_eq!(result["sort"], json!("desc"));
}

#[test]
fn from_query_overrides_defaults() {
    let im = InputMapping {
        from_query: {
            let mut m = std::collections::HashMap::new();
            m.insert("pageSize".into(), "size".into());
            m
        },
        defaults: {
            let mut m = std::collections::HashMap::new();
            m.insert("pageSize".into(), json!(20));
            m
        },
        ..Default::default()
    };
    let result = bff::server::mapping::merge_inputs(
        &im,
        &json!({"size": "50"}),
        &json!({}),
        &json!({}),
        &json!({}),
        &json!({}),
    );
    assert_eq!(result["pageSize"], json!("50"));
}

#[test]
fn from_body_json_path_extraction() {
    let im = InputMapping {
        from_body: {
            let mut m = std::collections::HashMap::new();
            m.insert("name".into(), "user.name".into());
            m.insert("email".into(), "user.email".into());
            m
        },
        ..Default::default()
    };
    let result = bff::server::mapping::merge_inputs(
        &im,
        &json!({}),
        &json!({"user": {"name": "Alice", "email": "alice@example.com"}}),
        &json!({}),
        &json!({}),
        &json!({}),
    );
    assert_eq!(result["name"], json!("Alice"));
    assert_eq!(result["email"], json!("alice@example.com"));
}

#[test]
fn from_body_root_extraction_with_dot() {
    let im = InputMapping {
        from_body: {
            let mut m = std::collections::HashMap::new();
            m.insert("payload".into(), ".".into());
            m
        },
        ..Default::default()
    };
    let result = bff::server::mapping::merge_inputs(
        &im,
        &json!({}),
        &json!({"data": "hello"}),
        &json!({}),
        &json!({}),
        &json!({}),
    );
    assert_eq!(result["payload"], json!({"data": "hello"}));
}

#[test]
fn from_header_source_extraction() {
    let im = InputMapping {
        from_header: {
            let mut m = std::collections::HashMap::new();
            m.insert("token".into(), "x-api-key".into());
            m.insert("trace".into(), "x-trace-id".into());
            m
        },
        ..Default::default()
    };
    let result = bff::server::mapping::merge_inputs(
        &im,
        &json!({}),
        &json!({}),
        &json!({"x-api-key": "abc123", "x-trace-id": "trace-001"}),
        &json!({}),
        &json!({}),
    );
    assert_eq!(result["token"], json!("abc123"));
    assert_eq!(result["trace"], json!("trace-001"));
}

#[test]
fn priority_query_over_default() {
    let im = InputMapping {
        from_query: {
            let mut m = std::collections::HashMap::new();
            m.insert("val".into(), "v".into());
            m
        },
        from_body: {
            let mut m = std::collections::HashMap::new();
            m.insert("val".into(), "bv".into());
            m
        },
        defaults: {
            let mut m = std::collections::HashMap::new();
            m.insert("val".into(), json!("default"));
            m
        },
        ..Default::default()
    };
    // query takes priority over body over defaults
    let result = bff::server::mapping::merge_inputs(
        &im,
        &json!({"v": "query_val"}),
        &json!({"bv": "body_val"}),
        &json!({}),
        &json!({}),
        &json!({}),
    );
    assert_eq!(result["val"], json!("query_val"));
}

#[test]
fn body_fallback_when_no_query() {
    let im = InputMapping {
        from_query: {
            let mut m = std::collections::HashMap::new();
            m.insert("val".into(), "v".into());
            m
        },
        from_body: {
            let mut m = std::collections::HashMap::new();
            m.insert("val".into(), "bv".into());
            m
        },
        defaults: {
            let mut m = std::collections::HashMap::new();
            m.insert("val".into(), json!("default"));
            m
        },
        ..Default::default()
    };
    // no query value → falls back to body
    let result = bff::server::mapping::merge_inputs(
        &im,
        &json!({"other": "x"}),
        &json!({"bv": "body_val"}),
        &json!({}),
        &json!({}),
        &json!({}),
    );
    assert_eq!(result["val"], json!("body_val"));
}

#[test]
fn default_fallback_when_no_query_or_body() {
    let im = InputMapping {
        from_query: {
            let mut m = std::collections::HashMap::new();
            m.insert("val".into(), "v".into());
            m
        },
        defaults: {
            let mut m = std::collections::HashMap::new();
            m.insert("val".into(), json!("fallback"));
            m
        },
        ..Default::default()
    };
    let result = bff::server::mapping::merge_inputs(
        &im,
        &json!({}),
        &json!({}),
        &json!({}),
        &json!({}),
        &json!({}),
    );
    assert_eq!(result["val"], json!("fallback"));
}

// ============================================================
// OutputMapping 测试
// ============================================================

#[test]
fn wrap_output_in_key() {
    let om = OutputMapping {
        wrap: Some("data".into()),
        ..Default::default()
    };
    let result = bff::server::mapping::apply_output_mapping(&om, json!({"id": 1}));
    assert_eq!(result, json!({"data": {"id": 1}}));
}

#[test]
fn rename_fields() {
    let om = OutputMapping {
        rename: {
            let mut m = std::collections::HashMap::new();
            m.insert("user_id".into(), "userId".into());
            m.insert("created_at".into(), "created".into());
            m
        },
        ..Default::default()
    };
    let result = bff::server::mapping::apply_output_mapping(
        &om,
        json!({"userId": 42, "created": "2024-01-01", "extra": "keep"}),
    );
    assert_eq!(result["user_id"], json!(42));
    assert_eq!(result["created_at"], json!("2024-01-01"));
    assert_eq!(result["extra"], json!("keep")); // kept because rename only renames matching keys
}

#[test]
fn pick_filter_whitelist() {
    let om = OutputMapping {
        pick: vec!["id".into(), "name".into()],
        ..Default::default()
    };
    let result = bff::server::mapping::apply_output_mapping(
        &om,
        json!({"id": 1, "name": "Alice", "secret": "hidden", "extra": "removed"}),
    );
    assert_eq!(result, json!({"id": 1, "name": "Alice"}));
}

#[test]
fn wrap_and_rename_combined() {
    let om = OutputMapping {
        wrap: Some("result".into()),
        rename: {
            let mut m = std::collections::HashMap::new();
            m.insert("uid".into(), "id".into());
            m
        },
        pick: vec!["id".into(), "name".into()],
        ..Default::default()
    };
    let result = bff::server::mapping::apply_output_mapping(
        &om,
        json!({"id": 1, "name": "Alice", "extra": "x"}),
    );
    // pick first: {"id": 1, "name": "Alice"}
    // rename: {"uid": 1, "name": "Alice"}
    // wrap: {"result": {"uid": 1, "name": "Alice"}}
    assert_eq!(result, json!({"result": {"uid": 1, "name": "Alice"}}));
}

#[test]
fn empty_output_mapping_passthrough() {
    let om = OutputMapping::default();
    let original = json!({"key": "value"});
    let result = bff::server::mapping::apply_output_mapping(&om, original.clone());
    assert_eq!(result, original);
}

#[test]
fn empty_input_mapping_returns_empty() {
    let im = InputMapping::default();
    let result = bff::server::mapping::merge_inputs(&im, &json!({}), &json!({}), &json!({}), &json!({}), &json!({}));
    assert!(result.is_object());
    assert!(result.as_object().unwrap().is_empty());
}
