//! QuickJS（rquickjs）脚本引擎：安全沙箱 + 隔离执行。
//!
//! - 通过 `spawn_blocking` 隔离，避免阻塞异步运行时；
//! - 中断回调限制执行时长，并设置内存 / 栈上限防止资源耗尽；
//! - 不注册任何 IO / 文件 / 模块加载能力，并移除 `eval` / `Function` 动态执行入口；
//! - `inputs` 全局变量传入上一步输出，最后一个表达式的值序列化为 JSON 返回。
use anyhow::Context as _;
use rquickjs::{Context, Ctx, Function, Runtime, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 脚本可用内存上限（QuickJS 分配器口径）
const MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
/// QuickJS 栈上限，防止深层递归打爆宿主线程栈
const MAX_STACK_BYTES: usize = 1024 * 1024;

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

    /// 以 JSON 作为 `inputs` 执行脚本，返回 JSON。
    pub async fn run_json(
        &self,
        script: &str,
        inputs: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let script = script.to_string();
        let max_duration = self.max_duration;
        let handle = tokio::task::spawn_blocking(move || {
            let runtime = Runtime::new().context("创建 QuickJS 运行时失败")?;
            runtime.set_memory_limit(MAX_MEMORY_BYTES);
            runtime.set_max_stack_size(MAX_STACK_BYTES);

            // 超时中断：QuickJS 解释器在函数调用 / 循环回跳时回调，
            // 返回 true 即抛出内部异常终止脚本执行。
            let start = Instant::now();
            let interrupted = Arc::new(AtomicBool::new(false));
            let flag = interrupted.clone();
            runtime.set_interrupt_handler(Some(Box::new(move || {
                if start.elapsed() > max_duration {
                    flag.store(true, Ordering::Relaxed);
                    true
                } else {
                    false
                }
            })));

            let context = Context::full(&runtime).context("创建 QuickJS 上下文失败")?;
            context.with(|ctx| {
                // 注入 inputs（经 JSON 字符串桥接，避免递归构造 JS 值）
                let inputs_json = serde_json::to_string(&inputs).context("inputs 序列化失败")?;
                let inputs_val = ctx
                    .json_parse(inputs_json)
                    .map_err(|e| anyhow::anyhow!("inputs 转换为脚本值失败: {}", e))?;
                ctx.globals()
                    .set("inputs", inputs_val)
                    .map_err(|e| anyhow::anyhow!("注入 inputs 失败: {}", e))?;

                // 注册 now()：返回当前 Unix 时间戳（秒，浮点数）
                let now_fn = Function::new(ctx.clone(), || -> f64 {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64()
                })
                .map_err(|e| anyhow::anyhow!("注册 now() 失败: {}", e))?;
                ctx.globals()
                    .set("now", now_fn)
                    .map_err(|e| anyhow::anyhow!("注册 now() 失败: {}", e))?;

                // 禁用动态执行入口，脚本只能使用内置纯函数与已注入的数据
                ctx.globals()
                    .remove("eval")
                    .map_err(|e| anyhow::anyhow!("禁用 eval 失败: {}", e))?;
                ctx.globals()
                    .remove("Function")
                    .map_err(|e| anyhow::anyhow!("禁用 Function 失败: {}", e))?;

                // 执行脚本，最后一个表达式的值作为结果
                let result: Value = ctx.eval(script).map_err(|e| {
                    if interrupted.load(Ordering::Relaxed) {
                        anyhow::anyhow!("脚本执行超时")
                    } else {
                        anyhow::anyhow!("脚本执行失败: {}", format_js_error(&ctx, e))
                    }
                })?;

                // 返回值经 JSON 桥接回 Rust
                let out = ctx
                    .json_stringify(&result)
                    .map_err(|e| anyhow::anyhow!("脚本返回值序列化失败: {}", e))?;
                let json: serde_json::Value = match out {
                    Some(s) => {
                        let s = s.to_string().context("脚本返回值非 UTF-8")?;
                        serde_json::from_str(&s).context("脚本返回值序列化失败")?
                    }
                    // undefined / function / symbol 等无法 JSON 化的值按 null 处理
                    None => serde_json::Value::Null,
                };
                Ok(json)
            })
        });
        match tokio::time::timeout(self.max_duration + Duration::from_secs(3), handle).await {
            Ok(joined) => joined.context("脚本任务 panic")?,
            Err(_) => anyhow::bail!("脚本执行超时（硬上限）"),
        }
    }
}

/// 将 rquickjs 错误格式化为可读信息：JS 异常时取出 message + stack。
fn format_js_error(ctx: &Ctx, err: rquickjs::Error) -> String {
    if matches!(err, rquickjs::Error::Exception) {
        let caught = ctx.catch();
        if let Some(exc) = caught.as_exception() {
            let message = exc.message().unwrap_or_default();
            let stack = exc.stack().unwrap_or_default();
            return if stack.is_empty() {
                message
            } else {
                format!("{}\n{}", message, stack)
            };
        }
        return format!("{:?}", caught);
    }
    err.to_string()
}
