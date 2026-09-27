//! W3C Trace Context（`traceparent`）解析与传播（O3）。
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

use axum::body::Body;
use axum::http::{HeaderName, Request};
use axum::middleware::Next;
use axum::response::Response;

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
pub async fn trace_context_middleware(mut req: Request<Body>, next: Next) -> Response {
    let ctx = req
        .headers()
        .get(TRACEPARENT)
        .and_then(|v| v.to_str().ok())
        .and_then(TraceContext::parse)
        // 存在上游上下文 → 本跳为子 span；否则新建根上下文
        .map(|parent| parent.child())
        .unwrap_or_else(TraceContext::new_root);

    if let Ok(value) = ctx.to_header().parse() {
        req.headers_mut().insert(TRACEPARENT, value);
    }

    let mut resp = next.run(req).await;
    if let Ok(value) = ctx.to_header().parse() {
        resp.headers_mut().insert(TRACEPARENT, value);
    }
    resp
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
}
