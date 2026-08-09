//! 输入/输出映射引擎。
//!
//! - `merge_inputs`: 按优先级从 query、body、header 提取参数，合并 defaults。
//! - `apply_output_mapping`: 按 OutputMapping 做字段重命名、包裹、过滤。
//!
//! 完整的 `extract_inputs` 需要 axum Request 上下文（读取 query string、body、headers），
//! 在 route_dispatcher 中调用。此处提供纯数据层面的合并函数供测试与组合使用。

use crate::config::{InputMapping, OutputMapping};
use serde_json::Value;
use std::collections::HashMap;

/// 合并多个来源的输入（优先级从低到高）。
///
/// 优先级：defaults < env < session < header < body < query
///
/// 每个 `from_*` 的键是目标变量名，值是来源路径。
/// 路径格式：
/// - `"."` 表示整个来源对象
/// - `"user.name"` 表示 JSON 路径（用 `.` 分隔）
pub fn merge_inputs(
    mapping: &InputMapping,
    query_json: &Value,
    body_json: &Value,
    header_json: &Value,
    session_json: &Value,
    env_json: &Value,
) -> Value {
    let mut result = serde_json::Map::new();

    // 1. defaults（最低优先级）
    for (key, val) in &mapping.defaults {
        result.insert(key.clone(), val.clone());
    }

    // 2. from_env
    apply_source(&mut result, &mapping.from_env, env_json);

    // 3. from_session
    apply_source(&mut result, &mapping.from_session, session_json);

    // 4. from_header
    apply_source(&mut result, &mapping.from_header, header_json);

    // 5. from_body
    apply_source(&mut result, &mapping.from_body, body_json);

    // 6. from_query（最高优先级）
    apply_source(&mut result, &mapping.from_query, query_json);

    Value::Object(result)
}

fn apply_source(
    result: &mut serde_json::Map<String, Value>,
    mapping: &HashMap<String, String>,
    source: &Value,
) {
    for (target_key, source_path) in mapping {
        let val = extract_json_path(source, source_path);
        if !val.is_null() {
            result.insert(target_key.clone(), val);
        }
    }
}

/// 简单 JSON 路径提取：`"."` 返回整个对象，`"a.b.c"` 返回深层值。
fn extract_json_path(source: &Value, path: &str) -> Value {
    if path == "." {
        return source.clone();
    }
    let mut current = source;
    for segment in path.split('.') {
        match current {
            Value::Object(map) => {
                current = map.get(segment).unwrap_or(&Value::Null);
            }
            _ => return Value::Null,
        }
    }
    current.clone()
}

/// 按 OutputMapping 转换输出：pick → rename → wrap。
pub fn apply_output_mapping(mapping: &OutputMapping, mut value: Value) -> Value {
    // 1. pick（白名单过滤）
    if !mapping.pick.is_empty() {
        if let Value::Object(map) = &value {
            let filtered: serde_json::Map<String, Value> = map
                .iter()
                .filter(|(k, _)| mapping.pick.contains(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            value = Value::Object(filtered);
        }
    }

    // 2. rename（rename map: new_name → original_name）
    if !mapping.rename.is_empty() {
        // 反转映射：original_name → new_name
        let reverse: HashMap<&String, &String> =
            mapping.rename.iter().map(|(new, old)| (old, new)).collect();
        if let Value::Object(map) = &value {
            let mut renamed = serde_json::Map::new();
            for (k, v) in map {
                let new_key = reverse
                    .get(k)
                    .map(|&s| s.clone())
                    .unwrap_or_else(|| k.clone());
                renamed.insert(new_key, v.clone());
            }
            value = Value::Object(renamed);
        }
    }

    // 3. wrap
    if let Some(wrap_key) = &mapping.wrap {
        value = Value::Object({
            let mut m = serde_json::Map::new();
            m.insert(wrap_key.clone(), value);
            m
        });
    }

    value
}
