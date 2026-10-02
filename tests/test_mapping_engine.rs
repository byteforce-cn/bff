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
    let result = bff::server::mapping::merge_inputs(
        &im,
        &json!({}),
        &json!({}),
        &json!({}),
        &json!({}),
        &json!({}),
    );
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
    let result = bff::server::mapping::merge_inputs(
        &im,
        &json!({}),
        &json!({}),
        &json!({}),
        &json!({}),
        &json!({}),
    );
    assert!(result.is_object());
    assert!(result.as_object().unwrap().is_empty());
}

// ============================================================
// from_env / from_path（修复回归）
//
// 背景：两处“接入层键名 vs 合并层路径”不一致导致特性从未生效：
// - from_env：上下文以**变量名**为键，而合并层把 `env.NAME` 当 JSON 路径查 `["env"]["NAME"]`；
// - from_path：提取阶段以**目标键**产出，而合并层用模板串（`path./api/{id}`）当 JSON 路径查。
// 修复前两者恒为 Null → 配置静默失效；以下用例锁定修复后的解析语义。
// ============================================================

#[test]
fn from_env_accepts_prefixed_and_bare_names() {
    let im = InputMapping {
        from_env: std::collections::HashMap::from([
            ("a".to_string(), "env.BFF_MAP_TEST_TOKEN".to_string()),
            ("b".to_string(), "BFF_MAP_TEST_TOKEN".to_string()),
            ("missing".to_string(), "env.BFF_MAP_TEST_NOPE".to_string()),
        ]),
        ..Default::default()
    };
    // 模拟 route_dispatcher::build_env_context 的输出：以变量名为键
    let env_json = json!({"BFF_MAP_TEST_TOKEN": "t0k"});
    let result = bff::server::mapping::merge_inputs_full(
        &im,
        &json!({}),
        &json!({}),
        &json!({}),
        &json!({}),
        &json!({}),
        &env_json,
    );
    assert_eq!(result["a"], "t0k", "env.NAME 形式必须生效（修复回归）");
    assert_eq!(result["b"], "t0k", "裸变量名形式保持可用");
    assert!(
        result.get("missing").is_none(),
        "未设置的变量不得注入（值为 Null 时跳过）"
    );
}

#[test]
fn from_env_wildcard_injects_whole_object() {
    let im = InputMapping {
        from_env: std::collections::HashMap::from([("all".to_string(), ".".to_string())]),
        ..Default::default()
    };
    let env_json = json!({"A": "1", "B": "2"});
    let result = bff::server::mapping::merge_inputs_full(
        &im,
        &json!({}),
        &json!({}),
        &json!({}),
        &json!({}),
        &json!({}),
        &env_json,
    );
    assert_eq!(result["all"], json!({"A": "1", "B": "2"}));
}

#[test]
fn from_path_merged_by_target_key() {
    let im = InputMapping {
        from_path: std::collections::HashMap::from([(
            "userId".to_string(),
            "path./api/users/{userId}".to_string(),
        )]),
        ..Default::default()
    };
    // 模拟 route_dispatcher 提取阶段产物：以目标键为键
    let path_json = json!({"userId": "42"});
    let result = bff::server::mapping::merge_inputs_full(
        &im,
        &json!({}),
        &json!({}),
        &json!({}),
        &path_json,
        &json!({}),
        &json!({}),
    );
    assert_eq!(result["userId"], "42", "from_path 必须生效（修复回归）");
}

#[test]
fn from_path_priority_lower_than_body_and_query() {
    // 优先级：defaults < env < session < header < path < body < query
    let im = InputMapping {
        from_path: std::collections::HashMap::from([(
            "id".to_string(),
            "path./api/{id}".to_string(),
        )]),
        from_body: std::collections::HashMap::from([("id".to_string(), "id".to_string())]),
        ..Default::default()
    };
    let path_json = json!({"id": "from-path"});
    let body_json = json!({"id": "from-body"});
    let result = bff::server::mapping::merge_inputs_full(
        &im,
        &json!({}),
        &body_json,
        &json!({}),
        &path_json,
        &json!({}),
        &json!({}),
    );
    assert_eq!(result["id"], "from-body", "body 应覆盖 path");
}
