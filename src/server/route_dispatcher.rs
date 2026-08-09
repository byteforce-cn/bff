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

/// 在 routes 中匹配请求（最长 path 前缀 + method 过滤）。
///
/// 返回匹配到的 RouteDef 引用，或 None。
pub fn match_route<'a>(routes: &'a [RouteDef], method: &str, path: &str) -> Option<&'a RouteDef> {
    routes
        .iter()
        .filter(|r| {
            path.starts_with(&r.path)
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

    // 按类型分发
    match &route.route_type {
        RouteType::Proxy => execute_proxy(state, route, session, req).await,
        RouteType::Pipeline => {
            let (parts, body) = req.into_parts();
            let body_bytes = axum::body::to_bytes(body, 1024 * 1024)
                .await
                .map_err(|e| AppError::bad_request(format!("读取请求体失败: {}", e)))?;
            let (session_json, env_json) = build_context_json(session).await;
            let inputs =
                extract_inputs_from_parts(&parts, &body_bytes, route, &session_json, &env_json);
            execute_pipeline(state, route, inputs).await
        }
        RouteType::Script => {
            let (parts, body) = req.into_parts();
            let body_bytes = axum::body::to_bytes(body, 1024 * 1024)
                .await
                .map_err(|e| AppError::bad_request(format!("读取请求体失败: {}", e)))?;
            let (session_json, env_json) = build_context_json(session).await;
            let inputs =
                extract_inputs_from_parts(&parts, &body_bytes, route, &session_json, &env_json);
            execute_script(state, route, inputs).await
        }
        RouteType::Static => execute_static(route),
    }
}

/// 从 Session 提取用户身份信息 + 收集环境变量，构建 JSON 上下文。
async fn build_context_json(session: &Session) -> (Value, Value) {
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

    // env_json: 所有环境变量
    let env_json = {
        let mut map = serde_json::Map::new();
        for (k, v) in std::env::vars() {
            map.insert(k, Value::String(v));
        }
        Value::Object(map)
    };

    (session_json, env_json)
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

    // 解析 headers
    let header_json = {
        let mut map = serde_json::Map::new();
        for (name, value) in &parts.headers {
            if let Ok(v) = value.to_str() {
                map.insert(name.as_str().to_string(), Value::String(v.to_string()));
            }
        }
        Value::Object(map)
    };

    mapping::merge_inputs(
        &route.input_mapping,
        &query_json,
        &body_json,
        &header_json,
        session_json,
        env_json,
    )
}

/// 从请求中按 InputMapping 提取参数。

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
        Ok(r) => Ok((r.status, Json(r.body)).into_response()),
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
        Ok(v) => Ok((StatusCode::OK, Json(v)).into_response()),
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
