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
    merge_inputs_full(
        mapping,
        query_json,
        body_json,
        header_json,
        &Value::Object(serde_json::Map::new()),
        session_json,
        env_json,
    )
}

/// 完整版合并（F9：新增 `from_path` 来源）。
///
/// 优先级：defaults < env < session < header < path < body < query
pub fn merge_inputs_full(
    mapping: &InputMapping,
    query_json: &Value,
    body_json: &Value,
    header_json: &Value,
    path_json: &Value,
    session_json: &Value,
    env_json: &Value,
) -> Value {
    let mut result = serde_json::Map::new();

    // 1. defaults（最低优先级）
    for (key, val) in &mapping.defaults {
        result.insert(key.clone(), val.clone());
    }

    // 2. from_env（第六轮修正：`env.NAME` 与裸 `NAME` 均可解析；
    //    原实现把 `env.NAME` 当 JSON 路径拆成 ["env","NAME"] → 恒 Null，文档推荐写法失效）
    apply_env_source(&mut result, &mapping.from_env, env_json);

    // 3. from_session
    apply_source(&mut result, &mapping.from_session, session_json);

    // 4. from_header
    apply_source(&mut result, &mapping.from_header, header_json);

    // 5. from_path（F9；第六轮修正：path_json 由 route_dispatcher 以**目标键**预提取，
    //    模板匹配已在提取阶段完成——原实现用模板串当 JSON 路径查询 → 恒 Null，from_path 从未生效）
    apply_path_source(&mut result, &mapping.from_path, path_json);

    // 6. from_body
    apply_source(&mut result, &mapping.from_body, body_json);

    // 7. from_query（最高优先级）
    apply_source(&mut result, &mapping.from_query, query_json);

    Value::Object(result)
}

/// 从实际请求路径按模板提取路径参数（F9）。
///
/// 模板示例：`/api/users/{userId}`；实际路径 `/api/users/42` → `Some("42")`。
/// 支持多段与多参数；`{name}` 段缺失或不匹配返回 None。
pub fn extract_path_param(request_path: &str, template: &str) -> Option<String> {
    let req_segs: Vec<&str> = request_path.trim_matches('/').split('/').collect();
    let tpl_segs: Vec<&str> = template.trim_matches('/').split('/').collect();
    if req_segs.len() != tpl_segs.len() {
        return None;
    }
    let mut captured: Option<String> = None;
    for (r, t) in req_segs.iter().zip(tpl_segs.iter()) {
        if t.starts_with('{') && t.ends_with('}') {
            if r.is_empty() {
                return None;
            }
            captured.get_or_insert_with(|| (*r).to_string());
        } else if r != t {
            return None;
        }
    }
    captured
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

/// `from_env` 专用合并。
///
/// `env_json` 的键是**变量名**（见 `route_dispatcher::build_env_context`），因此：
/// - 路径写 `env.NAME`（文档推荐）或裸 `NAME`，均按变量名取值；
/// - `"."` 通配返回整个 env 对象（S5：route_dispatcher 会对该形式告警）。
fn apply_env_source(
    result: &mut serde_json::Map<String, Value>,
    mapping: &HashMap<String, String>,
    env_json: &Value,
) {
    for (target_key, source_path) in mapping {
        let val = if source_path == "." {
            env_json.clone()
        } else {
            let name = source_path
                .strip_prefix("env.")
                .unwrap_or(source_path.as_str());
            env_json
                .get(name)
                .or_else(|| env_json.get(source_path.as_str()))
                .cloned()
                .unwrap_or(Value::Null)
        };
        if !val.is_null() {
            result.insert(target_key.clone(), val);
        }
    }
}

/// `from_path` 专用合并。
///
/// `path_json` 的键是**目标变量名**：模板匹配（`path./api/users/{id}`）在
/// `route_dispatcher::extract_inputs_from_parts` 阶段完成，此处只做取值。
fn apply_path_source(
    result: &mut serde_json::Map<String, Value>,
    mapping: &HashMap<String, String>,
    path_json: &Value,
) {
    for target_key in mapping.keys() {
        if let Some(val) = path_json.get(target_key) {
            if !val.is_null() {
                result.insert(target_key.clone(), val.clone());
            }
        }
    }
}

/// 简单 JSON 路径提取：`"."` 返回整个对象，`"a.b.c"` 返回深层值。
///
/// S5 注意：`"."` 为通配（返回整个来源对象）——对 `from_env` 不应使用，
/// route_dispatcher 会在使用该形式时打印告警，并限制为显式引用。
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

/// 解析 `status_map`（F10）：按响应体中的 `status` 字段（字符串）查表，
/// 缺省回退 `"default"` 键。返回 None 表示保持原状态码。
pub fn resolve_status(mapping: &OutputMapping, body: &Value) -> Option<u16> {
    if mapping.status_map.is_empty() {
        return None;
    }
    if let Some(s) = body.get("status").and_then(|v| v.as_str()) {
        if let Some(code) = mapping.status_map.get(s) {
            return Some(*code);
        }
    }
    mapping.status_map.get("default").copied()
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
