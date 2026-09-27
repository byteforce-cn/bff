//! SSE 流式透传：逐 chunk 从上游读取，逐 chunk 写入客户端响应体。
//!
//! 与普通 HTTP proxy（一次性 resp.bytes()）的区别：
//! - 使用 reqwest streaming response（resp.bytes_stream()）
//! - 通过 axum::body::Body::from_stream 构造流式响应
//! - 不缓冲完整响应体，实现低延迟逐块推送

use crate::middleware::circuit_breaker::CircuitBreakerRegistry;
use crate::utils::AppError;
use axum::body::Body;
use axum::http::HeaderMap;
use axum::response::Response;
use futures::StreamExt;

/// SSE 流式透传：从上游逐 chunk 读取，逐 chunk 写入客户端。
///
/// 适用场景：
/// - text/event-stream（SSE）
/// - 大文件下载
/// - 任何需要流式传输的 HTTP 响应
///
/// R15：传入 `completion` 时按**流的终态**计熔断（正常结束=成功；读取错误=失败），
/// 修正“流建立成功即记健康、中途夭折被忽略”的假健康问题。
///
/// S9：响应头走统一过滤策略（剥离 hop-by-hop / CORS 家族 / 默认剥离 set-cookie）。
#[allow(clippy::too_many_arguments)]
pub async fn sse_stream(
    http: &reqwest::Client,
    upstream_url: &str,
    method: reqwest::Method,
    body: Vec<u8>,
    auth_token: Option<String>,
    request_id: Option<&str>,
    extra_headers: &HeaderMap,
    completion: Option<(CircuitBreakerRegistry, String)>,
    forward_set_cookie: bool,
) -> Result<Response, AppError> {
    let mut out_req = http.request(method, upstream_url);

    if let Some(token) = auth_token {
        out_req = out_req.bearer_auth(token);
    }
    if let Some(rid) = request_id {
        out_req = out_req.header("x-request-id", rid);
    }
    if !body.is_empty() {
        out_req = out_req.body(body);
    }
    // 恢复原始实体头（Content-Type 等），覆盖 reqwest 对字节 body 的默认 octet-stream。
    // 注意：axum 用 http 1.x、reqwest 经 openidconnect 用 http 0.2，需按字节转换类型。
    for (name, value) in extra_headers {
        if let (Ok(n), Ok(v)) = (
            reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            out_req = out_req.header(n, v);
        }
    }

    let resp = out_req.send().await.map_err(|e| {
        tracing::warn!(%upstream_url, error = %e, "上游 SSE 连接失败");
        // S12：对外不暴露上游地址/错误细节
        AppError::bad_gateway("上游服务暂不可用")
    })?;

    let status = resp.status();
    let headers = resp.headers().clone();

    // 将响应体转为 Stream<Result<Vec<u8>, Error>>
    let byte_stream = resp.bytes_stream().map(|r| {
        r.map(|b| b.to_vec()).map_err(|e| {
            tracing::error!("SSE 流读取错误: {}", e);
            std::io::Error::other(e.to_string())
        })
    });

    // 统一装箱，按需包装完成态上报
    let body_stream: BoxStream<SseChunk> = match completion {
        Some((breakers, key)) => Box::pin(stream_with_outcome(byte_stream, breakers, key)),
        None => Box::pin(byte_stream),
    };
    let stream_body = Body::from_stream(body_stream);

    let mut builder = Response::builder().status(status.as_u16());

    // S9：统一响应头过滤（原实现连 hop-by-hop 都未过滤）
    for (k, v) in headers.iter() {
        if crate::server::proxy::should_strip_response_header(k.as_str(), forward_set_cookie) {
            continue;
        }
        if let Ok(val) = axum::http::HeaderValue::from_bytes(v.as_bytes()) {
            if let Ok(name) = k.as_str().parse::<axum::http::HeaderName>() {
                builder = builder.header(name, val);
            }
        }
    }

    builder
        .body(stream_body)
        .map_err(|e| AppError::internal(format!("构建 SSE 响应失败: {}", e)))
}

/// SSE 流元素类型。
type SseChunk = Result<Vec<u8>, std::io::Error>;
type BoxStream<T> = std::pin::Pin<Box<dyn futures::Stream<Item = T> + Send>>;

/// 包装流：按终态上报熔断器（读取错误 → failure；正常结束 → success）。
///
/// 客户端提前断开导致流被 drop 时不会上报——探针在途标记会因超时被复位，
/// 不会永久卡住半开状态。
fn stream_with_outcome<S>(
    stream: S,
    breakers: CircuitBreakerRegistry,
    key: String,
) -> impl futures::Stream<Item = SseChunk> + Send
where
    S: futures::Stream<Item = SseChunk> + Send + 'static,
{
    struct State {
        stream: BoxStream<SseChunk>,
        reporter: Option<(CircuitBreakerRegistry, String)>,
        live: bool,
    }
    let state = State {
        stream: Box::pin(stream),
        reporter: Some((breakers, key)),
        live: true,
    };
    futures::stream::unfold(state, |mut st| async move {
        if !st.live {
            return None;
        }
        match st.stream.next().await {
            Some(Ok(chunk)) => Some((Ok(chunk), st)),
            Some(Err(e)) => {
                if let Some((b, k)) = st.reporter.take() {
                    b.record_failure(&k).await;
                }
                st.live = false;
                Some((Err(e), st))
            }
            None => {
                if let Some((b, k)) = st.reporter.take() {
                    b.record_success(&k).await;
                }
                None
            }
        }
    })
}
