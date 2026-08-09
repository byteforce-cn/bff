//! 运行时 API：健康检查、指标、活跃会话、pipeline 试运行。
use crate::config::PipelineDef;
use crate::orchestration::dag;
use crate::orchestration::step::{execute_step, StepContext, StepOutput};
use crate::state::AppState;
use crate::utils::AppError;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::task::JoinSet;
use tower_sessions::session::Id;
use tower_sessions::SessionStore;

/// GET /admin/api/health
pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

/// GET /admin/api/metrics — Prometheus 文本格式
pub async fn metrics(State(state): State<AppState>) -> Response {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        state.prometheus.render(),
    )
        .into_response()
}

/// GET /admin/api/sessions — 活跃 Session 列表
pub async fn list_sessions(State(state): State<AppState>) -> Json<serde_json::Value> {
    let sessions: Vec<_> = state.sessions.read().await.values().cloned().collect();
    Json(serde_json::json!({ "sessions": sessions, "count": sessions.len() }))
}

/// DELETE /admin/api/sessions/:id — 撤销会话（从 MemoryStore 和内存 HashMap 中移除）
pub async fn delete_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Response, AppError> {
    let id: Id = session_id
        .parse()
        .map_err(|e| AppError::bad_request(format!("无效的会话 ID: {}", e)))?;

    // 从 MemoryStore 中删除 session 数据（使 cookie 立即失效）
    if let Err(e) = state.session_store.delete(&id).await {
        tracing::warn!(session_id = %id, error = %e, "从 session store 删除会话失败");
    }

    // 从管理端 HashMap 中删除
    let removed = state.sessions.write().await.remove(&session_id);
    if removed.is_some() {
        tracing::info!(session_id = %id, "管理员撤销会话");
        Ok((
            StatusCode::OK,
            Json(serde_json::json!({"status": "deleted"})),
        )
            .into_response())
    } else {
        // 可能已过期自动清理
        Err(AppError::not_found(format!("会话不存在: {}", session_id)))
    }
}

/// pipeline 试运行请求体
#[derive(Debug, Deserialize)]
pub struct PipelineTestRequest {
    /// 模板参数
    #[serde(default)]
    pub params: HashMap<String, String>,
    /// 模拟 session
    #[serde(default)]
    pub session: Option<serde_json::Value>,
    /// 模拟环境变量
    #[serde(default)]
    pub env: Option<serde_json::Value>,
    /// 试运行模式：跳过 HTTP 调用，仍执行 script
    #[serde(default)]
    pub dry_run: bool,
    /// 覆盖 pipeline 默认超时
    pub timeout_override: Option<String>,
}

/// POST /admin/api/pipelines/{name}/test — 试运行 pipeline，返回 step 级详情
pub async fn test_pipeline(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<PipelineTestRequest>,
) -> Result<Response, AppError> {
    let cfg = state.cfg();
    let def = cfg
        .pipelines
        .get(&name)
        .cloned()
        .ok_or_else(|| AppError::not_found(format!("pipeline 不存在: {}", name)))?;

    // 合并 session/env 到 params
    let mut test_params = req.params.clone();
    let mut session_injected = false;
    if let Some(ref sess) = req.session {
        if let Some(obj) = sess.as_object() {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    test_params
                        .entry(k.clone())
                        .or_insert_with(|| s.to_string());
                } else {
                    test_params
                        .entry(k.clone())
                        .or_insert_with(|| v.to_string());
                }
            }
            session_injected = true;
        }
    }
    if let Some(ref env) = req.env {
        if let Some(obj) = env.as_object() {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    test_params
                        .entry(k.clone())
                        .or_insert_with(|| s.to_string());
                } else {
                    test_params
                        .entry(k.clone())
                        .or_insert_with(|| v.to_string());
                }
            }
        }
    }

    // 构建带 dry_run 标记的 context
    let base_ctx = state.pipeline_executor.ctx();
    let ctx = StepContext {
        http: base_ctx.http.clone(),
        cache: base_ctx.cache.clone(),
        scripts: base_ctx.scripts.clone(),
        params: test_params.clone(),
        dry_run: req.dry_run,
    };

    // 执行 pipeline，收集 step 级详情
    let layers = dag::build_layers(&def)
        .map_err(|e| AppError::unprocessable(format!("pipeline 定义非法: {}", e)))?;
    let def = Arc::new(def.clone());
    let results: Arc<tokio::sync::RwLock<HashMap<String, StepOutput>>> =
        Arc::new(tokio::sync::RwLock::new(HashMap::new()));
    let step_details: Arc<tokio::sync::RwLock<Vec<serde_json::Value>>> =
        Arc::new(tokio::sync::RwLock::new(Vec::new()));

    // 审计日志
    let simulated_sub = req
        .session
        .as_ref()
        .and_then(|s| s.get("sub"))
        .and_then(|v| v.as_str())
        .unwrap_or("(none)");
    tracing::info!(
        event = "admin.pipeline.test",
        pipeline_name = name,
        simulated_sub = simulated_sub,
        dry_run = req.dry_run,
        "pipeline 试运行请求"
    );

    let total_start = Instant::now();

    let exec = async {
        for layer in layers {
            let mut set: JoinSet<anyhow::Result<(String, StepOutput, u64, bool)>> = JoinSet::new();
            for i in layer {
                let step = def.steps[i].clone();
                let params = test_params.clone();
                let ctx = ctx.clone();
                let results = results.clone();
                set.spawn(async move {
                    let inputs = results.read().await.clone();
                    let step_start = Instant::now();
                    let out =
                        execute_step(step.step_type, &step.config, &params, &inputs, &ctx).await?;
                    let duration_ms = step_start.elapsed().as_millis() as u64;
                    let is_dry_run = ctx.dry_run
                        && matches!(step.step_type, crate::config::StepType::HttpRequest);
                    Ok((step.id.clone(), out, duration_ms, is_dry_run))
                });
            }
            while let Some(joined) = set.join_next().await {
                match joined {
                    Ok(Ok((id, out, duration_ms, dry_run_step))) => {
                        let mut detail = serde_json::json!({
                            "id": id.clone(),
                            "status": out.status,
                            "duration_ms": duration_ms,
                        });
                        if dry_run_step {
                            detail["dry_run"] = serde_json::Value::Bool(true);
                        }
                        step_details.write().await.push(detail);
                        results.write().await.insert(id, out);
                    }
                    Ok(Err(e)) => {
                        set.abort_all();
                        return Err(e);
                    }
                    Err(e) => {
                        set.abort_all();
                        return Err(anyhow::anyhow!("step 任务异常: {}", e));
                    }
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    let timeout_dur = if let Some(ref t) = req.timeout_override {
        humantime::parse_duration(t)
            .map_err(|e| AppError::bad_request(format!("非法超时值: {}", e)))?
    } else {
        def.strategy.timeout
    };

    match tokio::time::timeout(timeout_dur, exec).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return Err(AppError::bad_gateway(e.to_string()));
        }
        Err(_) => {
            return Err(AppError::gateway_timeout(format!(
                "pipeline [{}] 整体超时",
                name
            )));
        }
    }

    let total_duration_ms = total_start.elapsed().as_millis() as u64;
    let results = results.read().await;

    // 聚合：优先最后一个 script step 输出
    let last_script = def
        .steps
        .iter()
        .rev()
        .find(|s| s.step_type == crate::config::StepType::Script);
    let body = match last_script.and_then(|s| results.get(&s.id)) {
        Some(out) => out.body.clone(),
        None => serde_json::to_value(&*results).unwrap_or(serde_json::Value::Null),
    };

    let steps = step_details.read().await.clone();

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "status": 200,
            "body": body,
            "steps": steps,
            "total_duration_ms": total_duration_ms,
            "session_injected": session_injected,
        })),
    )
        .into_response())
}
