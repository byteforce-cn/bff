//! OpenTelemetry（OTLP）追踪导出（O3）。
//!
//! 设计：
//! - **默认关闭**：`telemetry.otlp_endpoint` 为空时不注册任何导出层，
//!   仅保留 W3C `traceparent` 注入/传播（成本与行为与 O3 引入时一致）；
//! - 启用后：BatchSpanProcessor + OTLP/gRPC（tonic；TLS 走 rustls，不引入 openssl）导出
//!   `tracing` span；
//! - **跨服务衔接**：入站 `traceparent` 经 [`context_from_traceparent`] 转成远程父上下文，
//!   由 `middleware::trace_context::BffMakeSpan` 在请求 span 创建时 `set_parent`；
//!   出站/响应 `traceparent` 取自本跳 span 的 OTel 上下文（[`current_span_traceparent`]），
//!   从而 collector 中的 span 链与上游实际收到的 parent 完全一致；
//! - 采样：`ParentBased(TraceIdRatioBased(sample_ratio))`——有上游上下文时跟随上游
//!   采样位（W3C 语义），无上游时按比例采样；
//! - 进程优雅关停时调用 [`TelemetryHandle::shutdown`] flush 队列，避免丢尾部 span。

use crate::config::TelemetryConfig;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState};
use opentelemetry::{Context, KeyValue};
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider, Tracer};
use opentelemetry_sdk::Resource;
use std::time::Duration;

/// OTLP 导出超时（单批；超时仅影响导出，不阻塞业务请求）。
const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);

/// 已初始化的遥测句柄：持有 TracerProvider，负责关停 flush。
///
/// 进程退出前必须调用 [`TelemetryHandle::shutdown`]（main.rs 已接线）。
pub struct TelemetryHandle {
    provider: SdkTracerProvider,
    service_name: String,
}

impl TelemetryHandle {
    /// 供 `tracing_opentelemetry::layer().with_tracer(...)` 使用。
    pub fn tracer(&self) -> Tracer {
        self.provider.tracer(self.service_name.clone())
    }

    /// flush + 关停（同步阻塞）。
    ///
    /// ⚠️ 实现注意：SDK 的 `BatchSpanProcessor::shutdown()` 内部用
    /// `futures_executor::block_on` 等待后台导出任务完成——若在 **current_thread**
    /// 运行时的线程上调用会死锁。**异步上下文请一律使用 [`Self::shutdown_async`]**。
    pub fn shutdown(&self) {
        if let Err(e) = self.provider.shutdown() {
            tracing::warn!(error = %e, "OTel 关停失败，部分 span 可能未导出");
        }
    }

    /// flush + 关停（异步安全）：在线程池中执行阻塞式关停，
    /// 不阻塞运行时的 worker 线程（兼容 current_thread 与 multi_thread 运行时）。
    pub async fn shutdown_async(self) {
        // 注：spawn_blocking 需要处于 tokio 运行时上下文（main/测试均在运行时时调用）
        let handle = tokio::task::spawn_blocking(move || self.shutdown());
        if handle.await.is_err() {
            // join 失败（线程 panic）时静默；shutdown 内部已有告警
        }
    }
}

/// 按配置初始化 OTLP 导出；`otlp_endpoint` 为空时返回 `Ok(None)`（禁用）。
pub fn init(cfg: &TelemetryConfig) -> anyhow::Result<Option<TelemetryHandle>> {
    let Some(endpoint) = cfg.otlp_endpoint.as_deref() else {
        return Ok(None);
    };

    // 纵深防御：`AppConfig::load` 已校验，但直接调用方也应 fail-fast。
    // 注：tonic 的 Endpoint 对无 scheme 字符串是**宽松**的（连接期才失败），
    // 因此这里必须显式校验，而不是依赖 exporter.build()。
    let parsed = url::Url::parse(endpoint)
        .map_err(|e| anyhow::anyhow!("telemetry.otlp_endpoint 非法: {e}"))?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "http" | "https"),
        "telemetry.otlp_endpoint 必须为 http(s) URL: {endpoint}"
    );

    let exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .with_timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(|e| {
            anyhow::anyhow!("初始化 OTLP exporter 失败（telemetry.otlp_endpoint={endpoint}）: {e}")
        })?;

    let resource = Resource::builder_empty()
        .with_attributes([
            KeyValue::new("service.name", cfg.service_name.clone()),
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
            // 环境标识（BFF_ENV=prod/staging/...；未设置视为 dev）
            KeyValue::new(
                "deployment.environment",
                std::env::var("BFF_ENV").unwrap_or_else(|_| "dev".into()),
            ),
        ])
        .build();

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            cfg.sample_ratio,
        ))))
        .with_resource(resource)
        .build();

    tracing::info!(
        endpoint,
        service_name = %cfg.service_name,
        sample_ratio = cfg.sample_ratio,
        "OTel OTLP 导出已启用"
    );
    Ok(Some(TelemetryHandle {
        provider,
        service_name: cfg.service_name.clone(),
    }))
}

/// 由 W3C `traceparent` 构造 OTel **远程父上下文**（合法性校验与
/// `middleware::trace_context` 同源：版本 00、非零 trace/span id、16 进制）。
pub fn context_from_traceparent(value: &str) -> Option<Context> {
    let mut parts = value.trim().split('-');
    let version = parts.next()?;
    let trace_id = parts.next()?;
    let span_id = parts.next()?;
    let flags = parts.next()?;
    if version != "00" {
        return None;
    }
    // 显式长度/字符校验：`TraceId::from_hex`/`SpanId::from_hex` 基于整数解析，
    // 会**接受短串**（如 "4bf92f"），与 W3C 语义不符，必须在此拒绝。
    if trace_id.len() != 32
        || !trace_id.chars().all(|c| c.is_ascii_hexdigit())
        || span_id.len() != 16
        || !span_id.chars().all(|c| c.is_ascii_hexdigit())
    {
        return None;
    }
    let trace_id = TraceId::from_hex(trace_id).ok()?;
    let span_id = SpanId::from_hex(span_id).ok()?;
    let flags = u8::from_str_radix(flags, 16).ok()?;
    let trace_flags = if flags & 0x01 == 0x01 {
        TraceFlags::SAMPLED
    } else {
        TraceFlags::NOT_SAMPLED
    };
    let sc = SpanContext::new(trace_id, span_id, trace_flags, true, TraceState::default());
    if !sc.is_valid() {
        return None;
    }
    Some(Context::current().with_remote_span_context(sc))
}

/// 当前 tracing span 对应的 `traceparent`（OTel 导出层未注册或 span 未采样时返回 None）。
///
/// 返回 None 时调用方应回退到 `middleware::trace_context` 的手动上下文，
/// 保证「未启用 OTel」与「启用 OTel」两种形态下 W3C 传播语义一致。
pub fn current_span_traceparent() -> Option<String> {
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    let cx = tracing::Span::current().context();
    let sc = cx.span().span_context().clone();
    if !sc.is_valid() {
        return None;
    }
    Some(format!(
        "00-{}-{}-{:02x}",
        sc.trace_id(),
        sc.span_id(),
        sc.trace_flags().to_u8() & 0x01
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_from_traceparent_accepts_valid_and_flags() {
        let v = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let cx = context_from_traceparent(v).expect("合法 traceparent 应可解析");
        let sc = cx.span().span_context().clone();
        assert_eq!(
            sc.trace_id().to_string(),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
        assert_eq!(sc.span_id().to_string(), "00f067aa0ba902b7");
        assert!(sc.is_sampled());

        let unsampled = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00";
        let cx = context_from_traceparent(unsampled).unwrap();
        assert!(!cx.span().span_context().is_sampled());
    }

    #[test]
    fn context_from_traceparent_rejects_invalid() {
        assert!(context_from_traceparent("").is_none());
        assert!(context_from_traceparent("garbage").is_none());
        // 非 00 版本
        assert!(context_from_traceparent(
            "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        )
        .is_none());
        // 全零 trace id（非法）
        assert!(context_from_traceparent(
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01"
        )
        .is_none());
        // 全零 span id（非法）
        assert!(context_from_traceparent(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01"
        )
        .is_none());
        // 长度/字符非法
        assert!(context_from_traceparent("00-zzzz-00f067aa0ba902b7-01").is_none());
        assert!(context_from_traceparent("00-4bf92f-00f067aa0ba902b7-01").is_none());
    }
}
