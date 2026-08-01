//! Rhai 脚本引擎：安全沙箱 + 隔离执行。
//!
//! - 通过 `spawn_blocking` 隔离，避免阻塞异步运行时；
//! - 限制最大操作数与执行时长（progress 回调）；
//! - 不注册任何 IO / 文件能力，危险函数默认不可用；
//! - `inputs` 全局变量传入上一步输出，返回值序列化为 JSON。
use anyhow::Context;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_OPERATIONS: u64 = 1_000_000;
const PROGRESS_CHECK_EVERY: u64 = 10_000;

#[derive(Clone)]
pub struct ScriptEngine {
    max_duration: Duration,
}

impl Default for ScriptEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptEngine {
    pub fn new() -> Self {
        Self {
            max_duration: Duration::from_secs(2),
        }
    }

    /// 使用自定义最大执行时长创建引擎。
    pub fn new_with_max_duration(max_duration: Duration) -> Self {
        Self { max_duration }
    }

    fn build_engine(max_duration: Duration) -> rhai::Engine {
        let mut engine = rhai::Engine::new();
        engine.set_max_operations(MAX_OPERATIONS);
        // 禁用动态 eval，脚本只能使用内置纯函数与已注入的数据
        engine.disable_symbol("eval");

        // 注册 now()：返回当前 Unix 时间戳（秒，浮点数）
        engine.register_fn("now", || -> f64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()
        });

        let start = Instant::now();
        let ops = Arc::new(AtomicU64::new(0));
        engine.on_progress(move |_| {
            let n = ops.fetch_add(1, Ordering::Relaxed) + 1;
            if n % PROGRESS_CHECK_EVERY == 0 && start.elapsed() > max_duration {
                // 返回 Some 终止脚本执行
                return Some(rhai::Dynamic::from("脚本执行超时"));
            }
            None
        });
        engine
    }

    /// 以 JSON 作为 `inputs` 执行脚本，返回 JSON。
    pub async fn run_json(
        &self,
        script: &str,
        inputs: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let script = script.to_string();
        let max_duration = self.max_duration;
        let handle = tokio::task::spawn_blocking(move || {
            let engine = Self::build_engine(max_duration);
            let mut scope = rhai::Scope::new();
            let inputs_dyn = rhai::serde::to_dynamic(&inputs)
                .map_err(|e| anyhow::anyhow!("inputs 转换为脚本值失败: {}", e))?;
            scope.push("inputs", inputs_dyn);
            let result: rhai::Dynamic = engine
                .eval_with_scope(&mut scope, &script)
                .map_err(|e| anyhow::anyhow!("脚本执行失败: {}", e))?;
            let json: serde_json::Value =
                serde_json::to_value(&result).context("脚本返回值序列化失败")?;
            Ok(json)
        });
        match tokio::time::timeout(self.max_duration + Duration::from_secs(3), handle).await {
            Ok(joined) => joined.context("脚本任务 panic")?,
            Err(_) => anyhow::bail!("脚本执行超时（硬上限）"),
        }
    }
}
