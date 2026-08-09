//! 单个 step 的执行：http_request / script。
use crate::config::{StepConfig, StepType};
use crate::provider::CacheProvider;
use crate::scripting::ScriptEngine;
use crate::utils::template;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// step 输出。`body` 供后续 script step 通过 `inputs["step_id"].body` 引用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutput {
    pub status: u16,
    pub body: serde_json::Value,
}

/// step 执行上下文（注入共享资源）。
#[derive(Clone)]
pub struct StepContext {
    pub http: reqwest::Client,
    pub cache: Arc<dyn CacheProvider>,
    pub scripts: ScriptEngine,
    /// 路由层传入的参数（含 session/env 映射值），script step 可读取。
    pub params: HashMap<String, String>,
    /// dry_run 模式：跳过 HTTP 调用，仍执行 script step。
    pub dry_run: bool,
}

pub async fn execute_step(
    step_type: StepType,
    config: &StepConfig,
    params: &HashMap<String, String>,
    inputs: &HashMap<String, StepOutput>,
    ctx: &StepContext,
) -> anyhow::Result<StepOutput> {
    match step_type {
        StepType::HttpRequest => {
            if ctx.dry_run {
                Ok(StepOutput {
                    status: 200,
                    body: serde_json::json!({"dry_run": true, "url": config.url}),
                })
            } else {
                execute_http(config, params, ctx).await
            }
        }
        StepType::Script => execute_script(config, inputs, ctx).await,
    }
}

async fn execute_http(
    config: &StepConfig,
    params: &HashMap<String, String>,
    ctx: &StepContext,
) -> anyhow::Result<StepOutput> {
    let url_tpl = config.url.clone().context("http_request 缺少 url")?;
    let url = template::render(&url_tpl, params);
    let method = config.method.to_uppercase();

    // 缓存命中直接返回
    let cache_key = format!("pipeline:http:{}:{}", method, url);
    if config.cache_ttl.is_some() {
        if let Some(hit) = ctx.cache.get(&cache_key).await {
            if let Ok(out) = serde_json::from_slice::<StepOutput>(&hit) {
                metrics::counter!("bff_pipeline_cache_hit_total").increment(1);
                return Ok(out);
            }
        }
    }

    let method_parsed: reqwest::Method = method
        .parse()
        .with_context(|| format!("非法 HTTP 方法: {}", method))?;
    let mut req = ctx.http.request(method_parsed, &url);
    for (k, v) in &config.headers {
        req = req.header(k, template::render(v, params));
    }
    if let Some(body) = &config.body {
        req = req
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(template::render(body, params));
    }
    let timeout = config.timeout.unwrap_or(Duration::from_secs(3));

    let resp = tokio::time::timeout(timeout, req.send())
        .await
        .with_context(|| format!("下游请求超时: {}", url))?
        .with_context(|| format!("下游请求失败: {}", url))?;

    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.context("读取下游响应体失败")?;
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
    });
    let out = StepOutput { status, body };

    if let Some(ttl) = config.cache_ttl {
        if (200..300).contains(&status) {
            if let Ok(bytes) = serde_json::to_vec(&out) {
                ctx.cache.set(&cache_key, bytes, ttl).await;
            }
        }
    }
    Ok(out)
}

async fn execute_script(
    config: &StepConfig,
    inputs: &HashMap<String, StepOutput>,
    ctx: &StepContext,
) -> anyhow::Result<StepOutput> {
    let script = config.script.clone().context("script step 缺少 script")?;
    let mut inputs_json = serde_json::to_value(inputs).context("序列化 inputs 失败")?;

    // 将 params 合并入 inputs（优先级低于上游 step 输出，同名 key 时 step 输出覆盖）
    if let Some(obj) = inputs_json.as_object_mut() {
        for (k, v) in &ctx.params {
            obj.entry(k.clone())
                .or_insert_with(|| serde_json::Value::String(v.clone()));
        }
    }

    let value = ctx.scripts.run_json(&script, inputs_json).await?;
    Ok(StepOutput {
        status: 200,
        body: value,
    })
}
