//! 回归：IdP 无响应（黑洞）时，登录链路必须在超时窗口内失败而非永久挂起。
//!
//! 背景：历史实现（oauth2 4.x 的 `async_http_client`）无任何超时、每次调用新建客户端；
//! 现 OIDC 出网使用共享的 `AppState.oidc_http`（oauth2 5 起直传 `&reqwest::Client`）。

mod common;

use std::time::{Duration, Instant};

#[tokio::test]
async fn login_fails_fast_when_idp_hangs() {
    // 黑洞 IdP：接受 TCP 连接但永不响应任何字节
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock); // 持有连接、不读写
        }
    });

    let mut cfg = common::base_config();
    cfg.http_client.timeout = Some(Duration::from_secs(1));
    cfg.oidc.providers.push(bff::config::OidcProviderConfig {
        id: "blackhole".into(),
        display_name: String::new(),
        issuer_url: format!("http://{}", addr),
        client_id: "c".into(),
        client_secret: "s".into(),
        callback_path: "/auth/callback".into(),
        scopes: vec!["openid".into()],
        insecure_skip_id_token_verification: false,
        refresh_skew_secs: 60,
    });
    let bff = common::spawn_business(common::make_state(cfg)).await;

    let start = Instant::now();
    let resp = common::test_client()
        .get(format!("{}/login", bff))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("BFF 应对 IdP 超时后返回错误响应，而不是让请求挂起");

    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(4),
        "应在超时窗口内失败（<4s），实际 {:?}",
        elapsed
    );
    assert!(
        resp.status().is_server_error(),
        "IdP 不可达应返回 5xx，实际 {}",
        resp.status()
    );
}
