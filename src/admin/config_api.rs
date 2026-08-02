//! 配置管理 API：导出 / 导入（热重载）、OIDC provider、pipeline、脚本。
use crate::config::{AppConfig, OidcProviderConfig, PipelineDef};
use crate::state::AppState;
use crate::utils::AppError;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

/// GET /admin/api/config/export — 导出脱敏配置（YAML）
pub async fn export_config(State(state): State<AppState>) -> Result<Response, AppError> {
    let cfg = state.cfg().sanitized();
    let yaml = serde_yaml::to_string(&cfg)
        .map_err(|e| AppError::internal(format!("序列化失败: {}", e)))?;
    Ok((
        [(header::CONTENT_TYPE, "application/yaml; charset=utf-8")],
        yaml,
    )
        .into_response())
}

/// POST /admin/api/config/import — 导入配置（YAML 原文或 multipart），原子热重载
pub async fn import_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let yaml = extract_yaml(&headers, body).await?;
    let mut cfg: AppConfig = serde_yaml::from_str(&yaml)
        .map_err(|e| AppError::unprocessable(format!("配置解析失败: {}", e)))?;
    // 识别 `***` 哨兵并跳过覆盖（保留当前已注入的环境值，§4.3）
    cfg.merge_sensitive_secrets(&state.cfg());
    cfg.validate()
        .map_err(|e| AppError::unprocessable(format!("配置校验失败: {}", e)))?;
    state
        .replace_config(cfg)
        .map_err(|e| AppError::unprocessable(format!("配置应用失败: {}", e)))?;
    // provider 可能变化，清空 OIDC 客户端缓存
    for p in &state.cfg().oidc.providers {
        state.oidc_clients.invalidate(&p.id).await;
    }
    tracing::info!("配置已热重载");
    Ok((StatusCode::OK, Json(serde_json::json!({"status": "applied"}))).into_response())
}

/// 支持 multipart/form-data（file 字段）与原始 YAML body 两种上传方式。
async fn extract_yaml(headers: &HeaderMap, body: Bytes) -> Result<String, AppError> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if ct.starts_with("multipart/form-data") {
        let boundary = ct
            .split("boundary=")
            .nth(1)
            .ok_or_else(|| AppError::bad_request("multipart 缺少 boundary"))?;
        let body_str =
            String::from_utf8(body.to_vec()).map_err(|_| AppError::bad_request("body 非 UTF-8"))?;
        // 简易 multipart 解析：取第一个文件字段内容（两个 boundary 之间，去掉头部）
        let delim = format!("--{}", boundary);
        for part in body_str.split(&delim).skip(1) {
            let part = part.trim_start_matches('\r').trim_start_matches('\n');
            if part.starts_with("--") || part.is_empty() {
                continue;
            }
            if let Some(idx) = part.find("\r\n\r\n") {
                let content = &part[idx + 4..];
                return Ok(content.trim_end_matches('\r').trim_end_matches('\n').to_string());
            }
        }
        Err(AppError::bad_request("multipart 中未找到文件内容"))
    } else {
        String::from_utf8(body.to_vec()).map_err(|_| AppError::bad_request("body 非 UTF-8"))
    }
}

/// GET /admin/api/oidc/providers — 列出（脱敏）
pub async fn list_providers(State(state): State<AppState>) -> Json<serde_json::Value> {
    let cfg = state.cfg().sanitized();
    Json(serde_json::json!({ "providers": cfg.oidc.providers }))
}

/// PUT /admin/api/oidc/providers/{id} — 更新（不存在则新增）
pub async fn update_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(mut provider): Json<OidcProviderConfig>,
) -> Result<Response, AppError> {
    provider.id = id.clone();
    let mut cfg = state.cfg().as_ref().clone();
    match cfg.oidc.providers.iter_mut().find(|p| p.id == id) {
        Some(p) => *p = provider,
        None => cfg.oidc.providers.push(provider),
    }
    state
        .replace_config(cfg)
        .map_err(|e| AppError::unprocessable(format!("配置应用失败: {}", e)))?;
    state.oidc_clients.invalidate(&id).await;
    Ok((StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response())
}

/// GET /admin/api/pipelines
pub async fn list_pipelines(State(state): State<AppState>) -> Json<serde_json::Value> {
    let cfg = state.cfg();
    Json(serde_json::json!({ "pipelines": cfg.pipelines }))
}

/// POST /admin/api/pipelines?name=xxx — 新建/覆盖 pipeline（body 为 YAML 或 JSON）
pub async fn create_pipeline(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, AppError> {
    let name = q
        .get("name")
        .cloned()
        .ok_or_else(|| AppError::bad_request("缺少 ?name="))?;
    let def: PipelineDef = serde_yaml::from_slice(&body)
        .map_err(|e| AppError::unprocessable(format!("pipeline 解析失败: {}", e)))?;
    crate::orchestration::dag::validate_pipeline(&name, &def)
        .map_err(|e| AppError::unprocessable(e.to_string()))?;
    let mut cfg = state.cfg().as_ref().clone();
    cfg.pipelines.insert(name.clone(), def);
    state
        .replace_config(cfg)
        .map_err(|e| AppError::unprocessable(format!("配置应用失败: {}", e)))?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({"status": "created", "name": name}))).into_response())
}

/// DELETE /admin/api/pipelines/{name}
pub async fn delete_pipeline(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Response, AppError> {
    let mut cfg = state.cfg().as_ref().clone();
    if cfg.pipelines.remove(&name).is_none() {
        return Err(AppError::not_found(format!("pipeline 不存在: {}", name)));
    }
    state
        .replace_config(cfg)
        .map_err(|e| AppError::unprocessable(format!("配置应用失败: {}", e)))?;
    Ok((StatusCode::OK, Json(serde_json::json!({"status": "deleted"}))).into_response())
}

/// GET /admin/api/scripts — 列出内存脚本与 config/scripts 目录脚本
pub async fn list_scripts(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mut map = state.scripts.read().await.clone();
    if let Ok(rd) = std::fs::read_dir("config/scripts") {
        for e in rd.flatten() {
            if let Some(name) = e.file_name().to_str() {
                if name.ends_with(".rhai") {
                    if let Ok(content) = std::fs::read_to_string(e.path()) {
                        map.entry(name.to_string()).or_insert(content);
                    }
                }
            }
        }
    }
    Json(serde_json::json!({ "scripts": map }))
}

/// PUT /admin/api/scripts/{name} — 更新脚本（body 为脚本原文）
pub async fn update_script(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Bytes,
) -> Result<Response, AppError> {
    let script = String::from_utf8(body.to_vec())
        .map_err(|_| AppError::bad_request("脚本必须为 UTF-8 文本"))?;
    state.scripts.write().await.insert(name.clone(), script);
    Ok((StatusCode::OK, Json(serde_json::json!({"status": "ok", "name": name}))).into_response())
}

#[derive(Debug, Deserialize)]
pub struct EvalRequest {
    /// 直接给定脚本；缺省使用已存储的同名脚本
    pub script: Option<String>,
    #[serde(default)]
    pub inputs: serde_json::Value,
    /// 模拟 session（可选），注入到 inputs 顶层
    #[serde(default)]
    pub session: Option<serde_json::Value>,
    /// 模拟环境变量（可选），注入到 inputs 顶层
    #[serde(default)]
    pub env: Option<serde_json::Value>,
}

/// POST /admin/api/scripts/{name}/eval — 调试执行
pub async fn eval_script(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<EvalRequest>,
) -> Result<Response, AppError> {
    let script = match req.script {
        Some(s) => s,
        None => state
            .scripts
            .read()
            .await
            .get(&name)
            .cloned()
            .ok_or_else(|| AppError::not_found(format!("脚本不存在: {}", name)))?,
    };

    // 合并 inputs：session → env → 用户显式 inputs（优先级递增）
    let mut session_injected = false;
    let mut env_injected = false;

    let mut merged_inputs = serde_json::Map::new();

    // 1. 注入 session 字段（最低优先级）
    if let Some(ref sess) = req.session {
        if let Some(obj) = sess.as_object() {
            for (k, v) in obj {
                merged_inputs.insert(k.clone(), v.clone());
            }
            session_injected = true;
        }
    }

    // 2. 注入 env 字段
    if let Some(ref env) = req.env {
        if let Some(obj) = env.as_object() {
            for (k, v) in obj {
                merged_inputs.insert(k.clone(), v.clone());
            }
            env_injected = true;
        }
    }

    // 3. 用户显式 inputs（最高优先级，覆盖 session/env 中同名 key）
    if let Some(obj) = req.inputs.as_object() {
        for (k, v) in obj {
            merged_inputs.insert(k.clone(), v.clone());
        }
    } else if !req.inputs.is_null() {
        // 非 object 的 inputs → 保持原样（兼容旧行为）
        let engine = crate::scripting::ScriptEngine::new();
        return match engine.run_json(&script, req.inputs).await {
            Ok(v) => Ok(Json(
                serde_json::json!({"result": v, "debug": {"session_injected": false, "env_injected": false}}),
            )
            .into_response()),
            Err(e) => Err(AppError::unprocessable(e.to_string())),
        };
    }

    // 审计日志
    let simulated_sub = req
        .session
        .as_ref()
        .and_then(|s| s.get("sub"))
        .and_then(|v| v.as_str())
        .unwrap_or("(none)");
    tracing::info!(
        event = "admin.script.eval",
        script_name = name,
        simulated_sub = simulated_sub,
        session_injected = session_injected,
        env_injected = env_injected,
        "脚本 eval 请求"
    );

    let engine = crate::scripting::ScriptEngine::new();
    let inputs_value = serde_json::Value::Object(merged_inputs);
    match engine.run_json(&script, inputs_value).await {
        Ok(v) => Ok(Json(
            serde_json::json!({
                "result": v,
                "debug": {
                    "session_injected": session_injected,
                    "env_injected": env_injected
                }
            }),
        )
        .into_response()),
        Err(e) => Err(AppError::unprocessable(e.to_string())),
    }
}

// ============================================================
// 路由管理 API
// ============================================================

/// GET /admin/api/routes — 列出所有统一路由定义
pub async fn list_routes(State(state): State<AppState>) -> Json<serde_json::Value> {
    let cfg = state.cfg();
    Json(serde_json::json!({ "routes": cfg.routes }))
}

/// PUT /admin/api/routes — 全量替换统一路由定义
pub async fn update_routes(
    State(state): State<AppState>,
    Json(routes): Json<Vec<crate::config::RouteDef>>,
) -> Result<Response, AppError> {
    // 基本校验
    for (i, r) in routes.iter().enumerate() {
        if r.path.is_empty() {
            return Err(AppError::bad_request(format!("routes[{}].path 不能为空", i)));
        }
    }
    let mut cfg = state.cfg().as_ref().clone();
    cfg.routes = routes;
    state
        .replace_config(cfg)
        .map_err(|e| AppError::unprocessable(format!("配置应用失败: {}", e)))?;
    Ok((StatusCode::OK, Json(serde_json::json!({"status": "updated"}))).into_response())
}

/// GET /admin/api/routes/types — 返回支持的 RouteType 枚举
pub async fn list_route_types() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "types": ["proxy", "pipeline", "script", "static"]
    }))
}
