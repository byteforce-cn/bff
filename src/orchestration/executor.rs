//! DAG 执行器：分层并行调度、硬超时、fail_fast、per-step timeout。
use crate::config::{PipelineDef, StepConfig};
use crate::orchestration::dag;
use crate::orchestration::step::{execute_step, StepContext, StepOutput};
use crate::utils::AppError;
use axum::http::StatusCode;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;

#[derive(Clone)]
pub struct PipelineExecutor {
    ctx: StepContext,
    /// 默认 per-step 超时（当 step 未配置 timeout 时使用）
    pub default_step_timeout: Duration,
}

pub struct PipelineResult {
    /// 聚合结果 body
    pub body: serde_json::Value,
    pub status: StatusCode,
    /// continue 策略下失败的 step ID 列表
    pub failed_steps: Vec<String>,
}

impl PipelineExecutor {
    pub fn new(ctx: StepContext) -> Self {
        Self {
            ctx,
            default_step_timeout: Duration::from_secs(30),
        }
    }

    /// 获取内部 StepContext（用于构造测试用 executor）
    pub fn ctx(&self) -> &StepContext {
        &self.ctx
    }

    /// 执行 pipeline。`params` 为查询参数等模板变量。
    pub async fn run(
        &self,
        name: &str,
        def: &PipelineDef,
        params: HashMap<String, String>,
    ) -> Result<PipelineResult, AppError> {
        let layers = dag::build_layers(def)
            .map_err(|e| AppError::unprocessable(format!("pipeline 定义非法: {}", e)))?;
        let def = Arc::new(def.clone());
        let fail_fast = def.strategy.error_handling == "fail_fast";
        let results: Arc<tokio::sync::RwLock<HashMap<String, StepOutput>>> =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let failed_steps: Arc<tokio::sync::Mutex<Vec<String>>> =
            Arc::new(tokio::sync::Mutex::new(Vec::new()));

        let exec = async {
            for layer in layers {
                let mut set: JoinSet<anyhow::Result<(String, StepOutput), (String, anyhow::Error)>> = JoinSet::new();
                for i in layer {
                    let step = def.steps[i].clone();
                    let params = params.clone();
                    let mut ctx = self.ctx.clone();
                    ctx.params = params.clone();
                    let results = results.clone();
                    let step_timeout = step
                        .config
                        .timeout
                        .unwrap_or(self.default_step_timeout);
                    let failed_steps = failed_steps.clone();
                    let step_id = step.id.clone();

                    set.spawn(async move {
                        let inputs = results.read().await.clone();
                        // Per-step timeout (P1-8)
                        let step_fut = execute_step(
                            step.step_type,
                            &step.config,
                            &params,
                            &inputs,
                            &ctx,
                        );
                        match tokio::time::timeout(step_timeout, step_fut).await {
                            Ok(Ok(out)) => Ok((step_id.clone(), out)),
                            Ok(Err(e)) => Err((step_id.clone(), e)),
                            Err(_) => Err((
                                step_id.clone(),
                                anyhow::anyhow!(
                                    "step [{}] 执行超时（{:?}）",
                                    step_id, step_timeout
                                ),
                            )),
                        }
                    });
                }
                while let Some(joined) = set.join_next().await {
                    match joined {
                        Ok(Ok((id, out))) => {
                            results.write().await.insert(id, out);
                        }
                        Ok(Err((id, e))) => {
                            // 记录失败 step
                            failed_steps.lock().await.push(id.clone());
                            // fail_fast：中止剩余任务（JoinSet drop 时 abort）
                            set.abort_all();
                            if fail_fast {
                                return Err(classify_step_error(&e));
                            }
                            tracing::warn!(pipeline = name, step = id, error = %e, "step 失败（continue 策略）");
                        }
                        Err(e) => {
                            set.abort_all();
                            return Err(AppError::internal(format!("step 任务异常: {}", e)));
                        }
                    }
                }
            }
            Ok::<(), AppError>(())
        };

        // 硬超时：超时即中止整棵树（JoinSet drop → abort，无资源泄漏）
        match tokio::time::timeout(def.strategy.timeout, exec).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                metrics::counter!("bff_pipeline_timeout_total", "pipeline" => name.to_string())
                    .increment(1);
                return Err(AppError::gateway_timeout(format!(
                    "pipeline [{}] 整体超时（{:?}）",
                    name, def.strategy.timeout
                )));
            }
        }

        let results = results.read().await;
        let failed = failed_steps.lock().await.clone();
        // 优先返回最后一个 script step 的输出（聚合节点）
        let last_script = def
            .steps
            .iter()
            .rev()
            .find(|s| s.step_type == crate::config::StepType::Script);
        let (body, status) = match last_script.and_then(|s| results.get(&s.id)) {
            Some(out) => (out.body.clone(), out.status),
            None => {
                // 聚合：包含成功结果和失败信息
                let mut agg = serde_json::Map::new();
                for (id, out) in results.iter() {
                    agg.insert(id.clone(), out.body.clone());
                }
                if !failed.is_empty() {
                    agg.insert(
                        "_failed_steps".to_string(),
                        serde_json::Value::Array(
                            failed.iter().map(|s| serde_json::Value::String(s.clone())).collect(),
                        ),
                    );
                    agg.insert(
                        "_partial".to_string(),
                        serde_json::Value::Bool(true),
                    );
                }
                (serde_json::Value::Object(agg), 200)
            }
        };
        Ok(PipelineResult {
            body,
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
            failed_steps: failed,
        })
    }
}

/// 将 step 错误归类为合适的 HTTP 状态码。
fn classify_step_error(e: &anyhow::Error) -> AppError {
    let msg = e.to_string();
    if msg.contains("超时") {
        AppError::gateway_timeout(msg)
    } else {
        AppError::bad_gateway(msg)
    }
}
