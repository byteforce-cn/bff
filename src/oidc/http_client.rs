//! R13：带超时的 OIDC 出网 HTTP 客户端。
//!
//! 官方 `openidconnect::reqwest::async_http_client` **每次调用新建 `reqwest::Client`，
//! 且整个实现无任何超时设置**（vendored `oauth2-4.4.2/src/reqwest.rs`）：
//! IdP 建连后不返回会让 `/login`、`/auth/callback`、refresh、discovery 无限期挂起，
//! 并在途任务/会话锁持续堆积。本模块复用 BFF 的共享客户端（含 connect/总超时与连接池），
//! 以闭包形式注入 `request_async` / `discover_async`。

use openidconnect::reqwest::Error as OAuthReqwestError;
use openidconnect::{HttpRequest as OAuthRequest, HttpResponse as OAuthResponse};
use std::future::Future;
use std::pin::Pin;

/// OIDC 请求结果类型（oauth2 要求 `RE: Error + 'static`）。
pub type OidcHttpResult = Result<OAuthResponse, OAuthReqwestError<reqwest::Error>>;

/// 装箱后的异步结果（供 `request_async` / `discover_async` 的闭包返回）。
pub type OidcHttpFuture = Pin<Box<dyn Future<Output = OidcHttpResult> + Send>>;

/// 执行一次 OIDC 出网请求：不跟随重定向（防 SSRF）、复用连接池。
pub async fn call(http: &reqwest::Client, request: OAuthRequest) -> OidcHttpResult {
    let mut builder = http
        .request(request.method, request.url.as_str())
        .body(request.body);
    for (name, value) in &request.headers {
        builder = builder.header(name.as_str(), value.as_bytes());
    }

    let response = builder.send().await.map_err(OAuthReqwestError::Reqwest)?;
    let status_code = response.status();
    let headers = response.headers().to_owned();
    let body = response.bytes().await.map_err(OAuthReqwestError::Reqwest)?;
    Ok(OAuthResponse {
        status_code,
        headers,
        body: body.to_vec(),
    })
}

/// 可直接传给 `request_async` / `discover_async` 的闭包。
pub fn client_fn(http: reqwest::Client) -> impl Fn(OAuthRequest) -> OidcHttpFuture + Clone {
    move |req| {
        let http = http.clone();
        Box::pin(async move { call(&http, req).await })
    }
}
