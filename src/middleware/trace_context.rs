//! W3C Trace Context（`traceparent`）解析与传播。
//!
//! 原系统只有 `x-request-id`（UUID），**无 W3C `traceparent` 注入/透传** ——
//! 即便后续引入 OTel，跨 BFF→上游的 span 也无法自动衔接。本模块提供：
//! - 解析/生成 `traceparent`（版本 00）；
//! - 每跳生成新 `span_id`、保留 `trace_id` 与采样标志（符合 W3C 语义）；
//! - 中间件：入口确保请求/响应头均带合法 `traceparent`，
//!   请求头经代理透传规则（passthrough_headers）自动带到上游，
//!   再交给上游的 OTel/链路系统实现端到端衔接。
//!
//! 格式：`00-<32hex trace-id>-<16hex parent-id>-<2hex flags>`
//!
//! OTel 导出扩展：启用 `telemetry.otlp_endpoint` 后：
//! - 请求 span 由 [`BffMakeSpan`] 构造，并把入站 `traceparent` 设为 OTel 远程父上下文；
//! - 注入的 `traceparent` 优先取自本跳 span 的 OTel 上下文（[`crate::telemetry::current_span_traceparent`]），
//!   使 collector 中的 span 树与上游收到的 parent 严格一致；
//! - 未启用导出时自动回退到本模块的手动上下文（行为不变，既有测试不受影响）。

use axum::body::Body;
use axum::http::{HeaderName, Request};
use axum::middleware::Next;
use axum::response::Response;
use std::time::Duration;
use tower_http::trace::{DefaultOnResponse, MakeSpan, OnResponse};

/// 头部名称（静态）。
pub const TRACEPARENT: HeaderName = HeaderName::from_static("traceparent");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
    pub sampled: bool,
}

impl TraceContext {
    /// 生成新的根上下文（trace_id 随机、sampled=true）。
    pub fn new_root() -> Self {
        Self {
            trace_id: new_trace_id(),
            span_id: new_span_id(),
            sampled: true,
        }
    }

    /// 解析 `traceparent`；非法输入返回 None（调用方应生成新上下文）。
    pub fn parse(value: &str) -> Option<Self> {
        let parts: Vec<&str> = value.trim().split('-').collect();
        if parts.len() < 4 {
            return None;
        }
        // 仅支持版本 00（其它版本按规范应忽略）
        if parts[0] != "00" {
            return None;
        }
        let trace_id = parts[1];
        let parent_id = parts[2];
        if trace_id.len() != 32
            || !trace_id.chars().all(|c| c.is_ascii_hexdigit())
            || trace_id.chars().all(|c| c == '0')
        {
            return None;
        }
        if parent_id.len() != 16
            || !parent_id.chars().all(|c| c.is_ascii_hexdigit())
            || parent_id.chars().all(|c| c == '0')
        {
            return None;
        }
        let flags = u8::from_str_radix(parts[3], 16).ok()?;
        Some(Self {
            trace_id: trace_id.to_string(),
            span_id: new_span_id(),
            sampled: flags & 0x01 == 0x01,
        })
    }

    /// 生成子上下文：同 trace_id，新 span_id。
    pub fn child(&self) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            span_id: new_span_id(),
            sampled: self.sampled,
        }
    }

    /// 序列化为 `traceparent` 头值。
    pub fn to_header(&self) -> String {
        format!(
            "00-{}-{}-{:02x}",
            self.trace_id,
            self.span_id,
            if self.sampled { 0x01u8 } else { 0x00u8 }
        )
    }
}

fn new_trace_id() -> String {
    // UUID v4 的 simple 形式即 32 位十六进制
    uuid::Uuid::new_v4().simple().to_string()
}

fn new_span_id() -> String {
    format!("{:016x}", rand::random::<u64>())
}

/// 中间件：入口解析/生成 traceparent，并把本跳上下文写入请求头（供代理透传上游）。
///
/// ⚠️ 层序敏感：调用方（`server::business`）必须把它放在 TraceLayer **之内**、
/// 任何会创建子 span 的层（如 tower-sessions 的 `call` span）**之外**，
/// 以保证 `Span::current()` 即请求 span（否则 traceparent 的 span-id 与导出 span 不一致）。
pub async fn trace_context_middleware(mut req: Request<Body>, next: Next) -> Response {
    // 1. 手动上下文（无 OTel 导出层时的回退路径；含"入站非法则新建根"语义）
    let manual = req
        .headers()
        .get(TRACEPARENT)
        .and_then(|v| v.to_str().ok())
        .and_then(TraceContext::parse)
        // 存在上游上下文 → 本跳为子 span；否则新建根上下文
        .map(|parent| parent.child())
        .unwrap_or_else(TraceContext::new_root);

    // 2. 优先与导出的 OTel span 对齐（导出层未注册时返回 None → 回退手动上下文）：
    //    上游收到的 parent-id 即 collector 中本跳 span 的 span_id，跨服务链路可衔接。
    let value = crate::telemetry::current_span_traceparent().unwrap_or_else(|| manual.to_header());

    if let Ok(v) = value.parse() {
        req.headers_mut().insert(TRACEPARENT, v);
    }

    let mut resp = next.run(req).await;
    if let Ok(v) = value.parse() {
        resp.headers_mut().insert(TRACEPARENT, v);
    }
    resp
}

/// `TraceLayer` 的 span 构造器（替换默认实现）：
///
/// - span 名与字段遵循 OTel HTTP semconv（`http.request`/`http.method`/`http.target`/
///   `http.status_code`/`otel.kind=server`）；
/// - 入站 `traceparent` 经 [`crate::telemetry::context_from_traceparent`] 转为远程父上下文，
///   在 span 创建时 `set_parent`（导出层未注册时仅存于扩展，无副作用）。
#[derive(Clone, Copy, Debug, Default)]
pub struct BffMakeSpan;

impl<B> MakeSpan<B> for BffMakeSpan {
    fn make_span(&mut self, request: &Request<B>) -> tracing::Span {
        let span = tracing::info_span!(
            "http.request",
            "otel.kind" = "server",
            "http.method" = %request.method(),
            "http.target" = %request.uri().path(),
            "http.status_code" = tracing::field::Empty,
            // §10/§14：日志/追踪带站点名；无站点 Extension 时回退 "-"
            "bff.site" = %request_site_name(request),
        );
        if let Some(parent) = request
            .headers()
            .get(TRACEPARENT)
            .and_then(|v| v.to_str().ok())
            .and_then(crate::telemetry::context_from_traceparent)
        {
            use tracing_opentelemetry::OpenTelemetrySpanExt;
            // 失败（如父上下文非法）时保留无父 span，不影响请求处理
            if let Err(err) = span.set_parent(parent) {
                tracing::debug!(%err, "设置 OTel 父上下文失败");
            }
        }
        span
    }
}

/// §10/§14：从请求扩展读取站点名。`server::business` 以最外层的
/// `axum::Extension(Arc<SiteHandle>)` 注入，axum 0.7 直接以 `Arc<SiteHandle>` 为键
/// 存于 request extensions（`Extension<T>` 提取器读取的即 `T`），故 span 创建时可见；
/// 缺失（legacy 直接构造 / 层外）时回退 `"-"`。
fn request_site_name<B>(request: &Request<B>) -> String {
    request
        .extensions()
        .get::<std::sync::Arc<crate::site::SiteHandle>>()
        .map(|handle| handle.name.clone())
        .unwrap_or_else(|| "-".to_string())
}

/// 响应阶段补充记录 `http.status_code`（保留默认的完成日志）。
#[derive(Clone, Copy, Debug, Default)]
pub struct RecordStatusOnResponse;

impl<B> OnResponse<B> for RecordStatusOnResponse {
    fn on_response(self, response: &Response<B>, latency: Duration, span: &tracing::Span) {
        span.record("http.status_code", response.status().as_u16());
        DefaultOnResponse::new().on_response(response, latency, span);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_header() {
        let v = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let ctx = TraceContext::parse(v).expect("应能解析");
        assert_eq!(ctx.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert!(ctx.sampled);
        // 本跳 span_id 必须新生成（不等于上游 parent-id）
        assert_ne!(ctx.span_id, "00f067aa0ba902b7");
        // 回环解析：trace_id 保持不变，span_id 每跳重新生成
        let re = TraceContext::parse(&ctx.to_header()).expect("回环解析");
        assert_eq!(re.trace_id, ctx.trace_id);
        assert_ne!(re.span_id, ctx.span_id);
        assert_eq!(re.sampled, ctx.sampled);
    }

    #[test]
    fn reject_invalid_headers() {
        assert!(TraceContext::parse("").is_none());
        assert!(TraceContext::parse("garbage").is_none());
        assert!(
            TraceContext::parse("01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
                .is_none()
        );
        assert!(
            TraceContext::parse("00-00000000000000000000000000000000-00f067aa0ba902b7-01")
                .is_none()
        );
        assert!(
            TraceContext::parse("00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01")
                .is_none()
        );
        assert!(TraceContext::parse("00-zzzz-00f067aa0ba902b7-01").is_none());
    }

    #[test]
    fn child_keeps_trace_and_resamples() {
        let root = TraceContext::new_root();
        let child = root.child();
        assert_eq!(root.trace_id, child.trace_id);
        assert_ne!(root.span_id, child.span_id);
        assert!(child.sampled);
    }

    /// §10/§14：`BffMakeSpan` 构造的 span 带 `bff.site` 字段；请求无站点 Extension
    /// （legacy 直接构造 / Extension 层之外）时回退为 `"-"`。
    #[test]
    fn make_span_records_site_field_with_fallback() {
        use std::sync::{Arc, Mutex};
        use tracing::field::{Field, Visit};
        use tracing::span::{Attributes, Id};
        use tracing_subscriber::layer::{Context, SubscriberExt};

        #[derive(Clone)]
        struct CaptureSite(Arc<Mutex<Option<String>>>);

        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureSite {
            fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
                struct Visitor(Arc<Mutex<Option<String>>>);
                impl Visit for Visitor {
                    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                        if field.name() == "bff.site" {
                            *self.0.lock().unwrap() = Some(format!("{value:?}"));
                        }
                    }
                }
                attrs.record(&mut Visitor(self.0.clone()));
            }
        }

        let captured = Arc::new(Mutex::new(None));
        let subscriber = tracing_subscriber::registry().with(CaptureSite(captured.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let request = Request::builder().body(()).expect("构造请求");
            let _span = BffMakeSpan.make_span(&request);
        });
        assert_eq!(
            captured.lock().unwrap().as_deref(),
            Some("-"),
            "无站点 Extension 时 bff.site 应回退为 \"-\""
        );
    }
}
