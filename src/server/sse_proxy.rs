//! SSE 流式透传：逐 chunk 从上游读取，逐 chunk 写入客户端响应体。
//!
//! 与普通 HTTP proxy（一次性 resp.bytes()）的区别：
//! - 使用 reqwest streaming response（resp.bytes_stream()）
//! - 通过 axum::body::Body::from_stream 构造流式响应
//! - 不缓冲完整响应体，实现低延迟逐块推送

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
pub async fn sse_stream(
    http: &reqwest::Client,
    upstream_url: &str,
    method: reqwest::Method,
    body: Vec<u8>,
    auth_token: Option<String>,
    request_id: Option<&str>,
    extra_headers: &HeaderMap,
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

    let resp = out_req
        .send()
        .await
        .map_err(|e| AppError::bad_gateway(format!("上游 SSE 连接失败: {}", e)))?;

    let status = resp.status();
    let headers = resp.headers().clone();

    // 将响应体转为 Stream<Result<Vec<u8>, Error>>
    let byte_stream = resp.bytes_stream().map(|r| {
        r.map(|b| b.to_vec()).map_err(|e| {
            tracing::error!("SSE 流读取错误: {}", e);
            std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
        })
    });

    let stream_body = Body::from_stream(byte_stream);

    let mut builder = Response::builder().status(status.as_u16());

    // 透传必要的响应头
    for (k, v) in headers.iter() {
        let key = k.as_str();
        // Axum/Tower 会自动处理这些 hop-by-hop 头
        if matches!(
            key,
            "connection" | "transfer-encoding" | "keep-alive" | "upgrade"
        ) {
            continue;
        }
        if let Ok(val) = axum::http::HeaderValue::from_bytes(v.as_bytes()) {
            if let Ok(name) = key.parse::<axum::http::HeaderName>() {
                builder = builder.header(name, val);
            }
        }
    }

    builder
        .body(stream_body)
        .map_err(|e| AppError::internal(format!("构建 SSE 响应失败: {}", e)))
}

/// 标准一次性 HTTP 代理（与 sse_stream 平行，用于 proxy_mode="http"）。
pub async fn http_proxy(
    http: &reqwest::Client,
    upstream_url: &str,
    method: reqwest::Method,
    body: Vec<u8>,
    auth_token: Option<String>,
    extra_headers: &HeaderMap,
) -> Result<(axum::http::StatusCode, axum::http::HeaderMap, Vec<u8>), AppError> {
    let mut out_req = http.request(method, upstream_url);

    if let Some(token) = auth_token {
        out_req = out_req.bearer_auth(token);
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

    let resp = out_req
        .send()
        .await
        .map_err(|e| AppError::bad_gateway(format!("上游调用失败: {}", e)))?;

    let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    let mut headers = axum::http::HeaderMap::new();
    for (k, v) in resp.headers() {
        if let (Ok(name), Ok(val)) = (
            k.as_str().parse::<axum::http::HeaderName>(),
            axum::http::HeaderValue::from_bytes(v.as_bytes()),
        ) {
            headers.insert(name, val);
        }
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::bad_gateway(e.to_string()))?;

    Ok((status, headers, bytes.to_vec()))
}
