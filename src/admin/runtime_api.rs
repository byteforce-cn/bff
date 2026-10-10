//! 运行时 API：健康检查、指标、活跃会话、pipeline 试运行。
use crate::orchestration::dag;
use crate::orchestration::step::{execute_step, StepContext, StepOutput};
use crate::state::{AppState, SessionInfo};
use crate::utils::AppError;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::task::JoinSet;
use tower_sessions::session::Id;

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
///
/// 对索引中的当前条目以有界并发（≤16）加载 session record，派生
/// `providers`（record 中 `oidc:{provider}:tokens` 键）与 `sites`
/// （`effective_sites()` 中 `allowed_providers` 命中 providers 的站点名，
/// 去重并排序）。record 缺失（已过期）的条目跳过，交由 GC 清理（§10）；
/// 存储后端临时故障时保留内存索引行（`providers`/`sites` 可能为空）而非清空列表。
pub async fn list_sessions(State(state): State<AppState>) -> Json<serde_json::Value> {
    let entries: Vec<SessionInfo> = state.sessions.read().await.values().cloned().collect();
    let sites = state.cfg().effective_sites();

    let sessions: Vec<SessionInfo> = futures::stream::iter(entries.into_iter().map(|mut info| {
        let state = state.clone();
        let sites = sites.clone();
        async move {
            // 非法 ID（理论不可达）→ 跳过（下轮 GC 清理索引项）
            let parsed = info.id.parse::<Id>().ok()?;
            match state.session_store.load(&parsed).await {
                Ok(Some(record)) => {
                    let mut providers: Vec<String> = record
                        .data
                        .keys()
                        .filter_map(|k| {
                            k.strip_prefix("oidc:")
                                .and_then(|rest| rest.strip_suffix(":tokens"))
                        })
                        .map(str::to_string)
                        .collect();
                    providers.sort();
                    providers.dedup();
                    info.providers = providers;

                    let mut session_sites: Vec<String> = sites
                        .iter()
                        .filter(|s| {
                            s.allowed_providers
                                .iter()
                                .any(|p| info.providers.contains(p))
                        })
                        .map(|s| s.name.clone())
                        .collect();
                    session_sites.sort();
                    session_sites.dedup();
                    info.sites = session_sites;
                }
                // record 缺失（已过期）→ 跳过，交由 GC 清理
                Ok(None) => return None,
                // 存储后端临时故障（如 Redis 不可达）：不得清空整个列表；
                // 保留内存索引行（`providers`/`sites` 可能为空），下轮再试。
                Err(e) => {
                    tracing::warn!(id = %info.id, error = %e, "加载会话失败");
                }
            }
            Some(info)
        }
    }))
    .buffer_unordered(16)
    .filter_map(|entry| async move { entry })
    .collect()
    .await;

    Json(serde_json::json!({ "sessions": sessions, "count": sessions.len() }))
}

/// GET /admin/api/sites — 站点清单（§10）
///
/// 字段与 `ResolvedSite` 对齐；`logout_scope` 沿用配置的序列化形式
/// （`"global"` / `"site"`）。
pub async fn list_sites(State(state): State<AppState>) -> Json<serde_json::Value> {
    let sites: Vec<serde_json::Value> = state
        .cfg()
        .effective_sites()
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "port": s.port,
                "public_base_url": s.public_base_url,
                "default_provider": s.default_provider,
                "providers": s.allowed_providers,
                "session_profile": s.session_profile,
                "logout_scope": s.logout_scope,
                "legacy": s.legacy,
            })
        })
        .collect();
    Json(serde_json::json!({ "sites": sites }))
}

/// POST /admin/api/oidc/providers/:id/verify — 真实连通性校验（discovery）。
///
/// 返回 200 + `{ok, ...}`：连通性问题作为业务结果返回（非 5xx），便于 UI 展示。
pub async fn verify_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<axum::Json<serde_json::Value>, AppError> {
    let provider = state
        .cfg()
        .oidc
        .providers
        .iter()
        .find(|p| p.id == id)
        .cloned()
        .ok_or_else(|| AppError::not_found(format!("OIDC provider 不存在: {}", id)))?;

    let issuer = openidconnect::IssuerUrl::new(provider.issuer_url.clone())
        .map_err(|e| AppError::bad_request(format!("issuer_url 非法: {}", e)))?;
    let started = std::time::Instant::now();
    let result =
        openidconnect::core::CoreProviderMetadata::discover_async(issuer, &state.oidc_http).await;
    let latency_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(metadata) => Ok(axum::Json(serde_json::json!({
            "ok": true,
            "issuer": metadata.issuer().as_str(),
            "token_endpoint": metadata.token_endpoint().map(|u| u.to_string()),
            "jwks_uri": metadata.jwks_uri().as_str(),
            "latency_ms": latency_ms,
        }))),
        Err(e) => Ok(axum::Json(serde_json::json!({
            "ok": false,
            "error": e.to_string(),
            "latency_ms": latency_ms,
        }))),
    }
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
    // 同步清理该会话的 token exchange 缓存（交换得到的上游令牌必须随撤销失效）
    let cleared = crate::server::token_exchange::clear_session_cache(&state, &session_id).await;
    if cleared > 0 {
        tracing::info!(session_id = %session_id, cleared, "会话撤销清理 token exchange 缓存");
    }
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
