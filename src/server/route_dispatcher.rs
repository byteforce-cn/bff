//! 统一路由分发：匹配 RouteDef → 执行对应 handler + 输入/输出映射。
use crate::config::{RouteDef, RouteType};
use crate::oidc::handlers::{current_access_token, current_tokens};
use crate::server::mapping;
use crate::server::proxy;
use crate::state::AppState;
use crate::utils::AppError;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde_json::Value;
use std::collections::HashMap;
use tower_sessions::Session;

/// 在 routes 中匹配请求（最长 path 前缀 + 段边界 + method 过滤）。
///
/// F2：前缀匹配必须停在路径段边界——原实现 `starts_with` 会让 `/api` 命中 `/api-secret`，
/// 可能把越权请求转发到错误上游。
///
/// 返回匹配到的 RouteDef 引用，或 None。
pub fn match_route<'a>(routes: &'a [RouteDef], method: &str, path: &str) -> Option<&'a RouteDef> {
    routes
        .iter()
        .filter(|r| {
            let boundary = r.path.trim_end_matches('/');
            let matched = if boundary.is_empty() {
                path.starts_with('/')
            } else {
                path == boundary || path.starts_with(&format!("{}/", boundary))
            };
            matched
                && (r.methods.is_empty()
                    || r.methods.iter().any(|m| m.eq_ignore_ascii_case(method)))
        })
        .max_by_key(|r| r.path.len())
}

/// 统一路由入口：匹配 → 鉴权 → 分发 → 映射。
pub async fn dispatch(
    state: &AppState,
    route: &RouteDef,
    session: &Session,
    req: Request<Body>,
) -> Result<Response, AppError> {
    // 鉴权检查
    if route.auth_required {
        let _token = current_access_token(session)
            .await
            .ok_or_else(|| AppError::unauthorized("未登录或会话已过期"))?;
    }

    // R2：请求体上限统一读取配置（原实现硬编码 1 MiB，调大 body_limit 无效）
    let max_body = state.cfg().body_limit.max_bytes;

    // 按类型分发
    match &route.route_type {
        RouteType::Proxy => execute_proxy(state, route, session, req).await,
        RouteType::Pipeline => {
            let (parts, body) = req.into_parts();
            let body_bytes = axum::body::to_bytes(body, max_body)
                .await
                .map_err(|e| AppError::bad_request(format!("读取请求体失败: {}", e)))?;
            let (session_json, env_json) = build_context_json(session, &route.input_mapping).await;
            let inputs =
                extract_inputs_from_parts(&parts, &body_bytes, route, &session_json, &env_json);
            execute_pipeline(state, route, inputs).await
        }
        RouteType::Script => {
            let (parts, body) = req.into_parts();
            let body_bytes = axum::body::to_bytes(body, max_body)
                .await
                .map_err(|e| AppError::bad_request(format!("读取请求体失败: {}", e)))?;
            let (session_json, env_json) = build_context_json(session, &route.input_mapping).await;
            let inputs =
                extract_inputs_from_parts(&parts, &body_bytes, route, &session_json, &env_json);
            execute_script(state, route, inputs).await
        }
        RouteType::Static => execute_static(route),
    }
}

/// 从 Session 提取用户身份信息 + 按需收集环境变量，构建 JSON 上下文。
///
/// S5：
/// - 仅当 `input_mapping.from_env` 非空时才收集环境变量（原实现在每个
///   pipeline/script 请求上无条件克隆全量环境变量，是请求路径上的稳定开销）；
/// - 只注入**显式引用**的变量名，不再默认提供全量环境；
/// - 显式写 `env: { ... : "." }` 通配时打印告警（保留逃生舱，但可审计）。
async fn build_context_json(
    session: &Session,
    input_mapping: &crate::config::InputMapping,
) -> (Value, Value) {
    // session_json: sub, provider, access_token
    let session_json = if let Some(tokens) = current_tokens(session).await {
        let mut map = serde_json::Map::new();
        map.insert("sub".into(), Value::String(tokens.sub.clone()));
        map.insert("provider".into(), Value::String(tokens.provider.clone()));
        if let Ok(at) = tokens.access_token() {
            map.insert("access_token".into(), Value::String(at));
        }
        Value::Object(map)
    } else {
        Value::Object(serde_json::Map::new())
    };

    let env_json = build_env_context(input_mapping);
    (session_json, env_json)
}

/// S5：按 `from_env` 映射构建最小环境变量上下文。
fn build_env_context(input_mapping: &crate::config::InputMapping) -> Value {
    if input_mapping.from_env.is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    let mut map = serde_json::Map::new();
    for source_path in input_mapping.from_env.values() {
        if source_path == "." {
            tracing::warn!(
                "input_mapping.from_env 使用了 '.' 通配：将注入全部环境变量，建议改为显式变量名（如 env.MY_VAR）"
            );
            for (k, v) in std::env::vars() {
                map.insert(k, Value::String(v));
            }
        } else {
            let name = source_path
                .strip_prefix("env.")
                .unwrap_or(source_path.as_str());
            if let Ok(v) = std::env::var(name) {
                map.insert(name.to_string(), Value::String(v));
            }
        }
    }
    Value::Object(map)
}

/// 从请求 parts 和 body bytes 中按 InputMapping 提取参数。
fn extract_inputs_from_parts(
    parts: &axum::http::request::Parts,
    body_bytes: &[u8],
    route: &RouteDef,
    session_json: &Value,
    env_json: &Value,
) -> Value {
    // 解析 query string
    let query_json = {
        let query = parts.uri.query().unwrap_or("");
        let mut map = serde_json::Map::new();
        for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
            map.insert(k.into_owned(), Value::String(v.into_owned()));
        }
        Value::Object(map)
    };

    // 解析 body（JSON）
    let body_json = if route.input_mapping.from_body.is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_slice(body_bytes).unwrap_or(Value::Object(serde_json::Map::new()))
    };

    // 解析 headers（S5：过滤敏感头，避免 cookie/authorization 进入脚本/编排可见上下文）
    let header_json = {
        const SENSITIVE: &[&str] = &[
            "cookie",
            "authorization",
            "proxy-authorization",
            "x-admin-token",
            "set-cookie",
        ];
        let mut map = serde_json::Map::new();
        for (name, value) in &parts.headers {
            if SENSITIVE.contains(&name.as_str()) {
                continue;
            }
            if let Ok(v) = value.to_str() {
                map.insert(name.as_str().to_string(), Value::String(v.to_string()));
            }
        }
        Value::Object(map)
    };

    // F9：from_path 参数提取（模板如 path./api/users/{userId}）
    let path_json = {
        let request_path = parts.uri.path();
        let mut map = serde_json::Map::new();
        for (target_key, template) in &route.input_mapping.from_path {
            let tpl = template.strip_prefix("path.").unwrap_or(template.as_str());
            if let Some(v) = mapping::extract_path_param(request_path, tpl) {
                map.insert(target_key.clone(), Value::String(v));
            }
        }
        Value::Object(map)
    };

    mapping::merge_inputs_full(
        &route.input_mapping,
        &query_json,
        &body_json,
        &header_json,
        &path_json,
        session_json,
        env_json,
    )
}

/// Proxy 执行：委托给现有 proxy_handler 逻辑。
async fn execute_proxy(
    state: &AppState,
    route: &RouteDef,
    session: &Session,
    req: Request<Body>,
) -> Result<Response, AppError> {
    let upstream = route
        .config
        .upstream
        .as_deref()
        .ok_or_else(|| AppError::bad_request("proxy 路由缺少 upstream"))?;

    proxy::forward_request(state, session, route, upstream, req).await
}

/// Pipeline 执行：引用 pipeline 注册表或内联执行。
async fn execute_pipeline(
    state: &AppState,
    route: &RouteDef,
    inputs: Value,
) -> Result<Response, AppError> {
    let def = if let Some(name) = &route.config.pipeline {
        // 引用已注册 pipeline
        state
            .cfg()
            .pipelines
            .get(name)
            .cloned()
            .ok_or_else(|| AppError::not_found(format!("pipeline 不存在: {}", name)))?
    } else if let Some(inline) = &route.config.pipeline_inline {
        inline.clone()
    } else {
        return Err(AppError::bad_request(
            "pipeline 路由缺少 pipeline 或 pipeline_inline",
        ));
    };

    // 将 inputs 转为 HashMap<String, String>
    let params: HashMap<String, String> = inputs_to_string_map(&inputs);

    let start = std::time::Instant::now();
    let result = state
        .pipeline_executor
        .run(
            route.config.pipeline.as_deref().unwrap_or("inline"),
            &def,
            params,
        )
        .await;

    let pipeline_name = route
        .config
        .pipeline
        .as_deref()
        .unwrap_or("inline")
        .to_string();
    metrics::histogram!("bff_pipeline_duration_seconds", "pipeline" => pipeline_name)
        .record(start.elapsed().as_secs_f64());

    match result {
        Ok(r) => {
            // F1/F10：执行 output_mapping（pick/rename/wrap）与 status_map
            let mapped = mapping::apply_output_mapping(&route.output_mapping, r.body);
            let status = mapping::resolve_status(&route.output_mapping, &mapped)
                .unwrap_or(r.status.as_u16());
            let status = StatusCode::from_u16(status).unwrap_or(r.status);
            Ok((status, Json(mapped)).into_response())
        }
        Err(e) => Err(e),
    }
}

/// Script 执行：引用脚本注册表或内联执行。
async fn execute_script(
    state: &AppState,
    route: &RouteDef,
    inputs: Value,
) -> Result<Response, AppError> {
    let script = if let Some(name) = &route.config.script {
        // 先从内存注册表查找
        if let Some(s) = state.scripts.read().await.get(name).cloned() {
            s
        } else {
            // 尝试从 config/scripts 目录读取
            let path = format!("config/scripts/{}", name);
            std::fs::read_to_string(&path)
                .map_err(|_| AppError::not_found(format!("脚本不存在: {}", name)))?
        }
    } else if let Some(inline) = &route.config.script_inline {
        inline.clone()
    } else {
        return Err(AppError::bad_request(
            "script 路由缺少 script 或 script_inline",
        ));
    };

    let engine = crate::scripting::ScriptEngine::new();
    match engine.run_json(&script, inputs).await {
        Ok(v) => {
            // F1/F10：执行 output_mapping 与 status_map
            let mapped = mapping::apply_output_mapping(&route.output_mapping, v);
            let status = mapping::resolve_status(&route.output_mapping, &mapped).unwrap_or(200);
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
            Ok((status, Json(mapped)).into_response())
        }
        Err(e) => Err(AppError::unprocessable(e.to_string())),
    }
}

/// Static 执行：返回固定响应。
fn execute_static(route: &RouteDef) -> Result<Response, AppError> {
    let status = StatusCode::from_u16(route.config.status.unwrap_or(200))
        .map_err(|_| AppError::internal("非法状态码"))?;
    let body = route.config.body.clone().unwrap_or(Value::Null);
    let mut builder = Response::builder().status(status);

    // 自定义 headers
    if let Some(headers) = &route.config.headers {
        for (k, v) in headers {
            if let (Ok(name), Ok(val)) = (
                k.as_str().parse::<axum::http::HeaderName>(),
                axum::http::HeaderValue::from_str(v),
            ) {
                builder = builder.header(name, val);
            }
        }
    }

    let resp = Json(body).into_response();
    // 简单返回：将 status 应用上去
    let (_, body) = resp.into_parts();
    builder
        .body(body)
        .map_err(|e| AppError::internal(e.to_string()))
}

/// 将 Value::Object 转为 HashMap<String, String>（用于 pipeline 参数）。
fn inputs_to_string_map(inputs: &Value) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Value::Object(obj) = inputs {
        for (k, v) in obj {
            match v {
                Value::String(s) => {
                    map.insert(k.clone(), s.clone());
                }
                Value::Number(n) => {
                    map.insert(k.clone(), n.to_string());
                }
                other => {
                    map.insert(k.clone(), other.to_string());
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{InputMapping, OutputMapping, RouteTypeConfig};

    fn r(path: &str, methods: &[&str]) -> RouteDef {
        RouteDef {
            path: path.into(),
            methods: methods.iter().map(|s| s.to_string()).collect(),
            description: String::new(),
            auth_required: false,
            route_type: RouteType::Static,
            config: RouteTypeConfig::default(),
            input_mapping: InputMapping::default(),
            output_mapping: OutputMapping::default(),
        }
    }

    #[test]
    fn match_route_exact_and_boundary() {
        let routes = vec![r("/api/dt", &[]), r("/api/dt-secret", &[])];
        assert_eq!(
            match_route(&routes, "GET", "/api/dt").unwrap().path,
            "/api/dt"
        );
        assert_eq!(
            match_route(&routes, "GET", "/api/dt/1").unwrap().path,
            "/api/dt"
        );
        // F2：段边界——/api/dt 不得命中 /api/dt-secret
        assert_eq!(
            match_route(&routes, "GET", "/api/dt-secret").unwrap().path,
            "/api/dt-secret"
        );
        assert!(match_route(&routes, "GET", "/api/dt-secretx").is_none());
    }

    #[test]
    fn match_route_longest_prefix_wins() {
        let routes = vec![r("/api", &[]), r("/api/users", &[])];
        assert_eq!(
            match_route(&routes, "GET", "/api/users/1").unwrap().path,
            "/api/users"
        );
        assert_eq!(
            match_route(&routes, "GET", "/api/orders").unwrap().path,
            "/api"
        );
    }

    #[test]
    fn match_route_method_filter_is_case_insensitive() {
        let routes = vec![r("/api/only-post", &["POST"])];
        assert!(match_route(&routes, "GET", "/api/only-post").is_none());
        assert!(
            match_route(&routes, "post", "/api/only-post").is_some(),
            "方法匹配应大小写不敏感"
        );
        let any = vec![r("/api/any", &[])];
        assert!(
            match_route(&any, "DELETE", "/api/any").is_some(),
            "空 methods = 全部放行"
        );
    }

    #[test]
    fn match_route_trailing_slash_config_normalized() {
        let routes = vec![r("/api/", &[])];
        assert!(
            match_route(&routes, "GET", "/api").is_some(),
            "配置尾部斜杠应归一"
        );
        assert!(match_route(&routes, "GET", "/api/x").is_some());
    }

    #[test]
    fn inputs_to_string_map_converts_all_scalar_kinds() {
        let v = serde_json::json!({"s": "str", "n": 42, "o": {"k": 1}, "b": true});
        let m = inputs_to_string_map(&v);
        assert_eq!(m.get("s").unwrap(), "str");
        assert_eq!(m.get("n").unwrap(), "42");
        assert!(m.get("o").unwrap().contains('k'));
        assert_eq!(m.get("b").unwrap(), "true");
        // 非对象输入 → 空表（不 panic）
        assert!(inputs_to_string_map(&serde_json::json!("x")).is_empty());
    }

    #[test]
    fn build_env_context_respects_explicit_and_wildcard() {
        std::env::set_var("BFF_UT_ENV_A", "va");

        // 空映射 → 不收集（S5：非显式引用不注入）
        assert_eq!(
            build_env_context(&InputMapping::default()),
            serde_json::json!({})
        );

        // 显式引用（带 / 不带 env. 前缀均可）→ 上下文以**变量名**为键
        let mut m = InputMapping::default();
        m.from_env.insert("a".into(), "env.BFF_UT_ENV_A".into());
        m.from_env.insert("b".into(), "BFF_UT_ENV_A".into());
        let ctx = build_env_context(&m);
        assert_eq!(ctx["BFF_UT_ENV_A"], "va");

        // 未设置的变量不注入
        let mut m2 = InputMapping::default();
        m2.from_env
            .insert("c".into(), "env.BFF_UT_ENV_DOES_NOT_EXIST".into());
        assert!(build_env_context(&m2).as_object().unwrap().is_empty());

        // "." 通配：注入全量（含刚设置的变量）
        let mut m3 = InputMapping::default();
        m3.from_env.insert("all".into(), ".".into());
        assert_eq!(build_env_context(&m3)["BFF_UT_ENV_A"], "va");
    }
}
