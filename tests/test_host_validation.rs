//! Task 12：Host 白名单校验中间件（§6.3）——伪造 Host 返回 421 Misdirected Request。
//!
//! 验收：
//! - 多站点 prod 语义：白名单（`server_names` ∪ public 主机）放行、未知 Host 421；
//! - `/live`、`/ready` 任意 Host 无条件豁免；
//! - 配置 `public_base_url` 后 loopback Host 也拒绝（prod 语义）；
//! - legacy 未启用 `enforce_host` 行为冻结；启用后全路径应用白名单；
//! - Host 缺失 / 不可解析 → 421。
//!
//! prod 语义配置自行构造（`server_names` + `public_base_url`），
//! 不依赖 `common::multisite_config`（dev 语义，无 public_base_url）。

mod common;

use bff::config::{AppConfig, LogoutScope, SiteConfig, SiteOidcConfig};
use common::{base_config, make_state, spawn_business, spawn_site, test_client};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// prod 语义站点：显式 `server_names` + `public_base_url`（无 OIDC provider 绑定，
/// 本任务只验证 Host 层，不涉及登录）。
fn prod_site(
    name: &str,
    port: u16,
    server_names: &[&str],
    public_base_url: Option<&str>,
) -> SiteConfig {
    SiteConfig {
        name: name.into(),
        port,
        bind: "0.0.0.0".into(),
        server_names: server_names.iter().map(|s| s.to_string()).collect(),
        public_base_url: public_base_url.map(|s| s.to_string()),
        session_profile: "default".into(),
        spa: None,
        oidc: SiteOidcConfig::default(),
        logout_scope: LogoutScope::Global,
        security_headers: None,
    }
}

/// 多站点配置：占位全局端口 + 单个 prod 语义站点。
fn prod_config(site: SiteConfig) -> AppConfig {
    let mut cfg = base_config();
    cfg.server.business_port = 8080;
    cfg.server.admin_port = 8443;
    cfg.sites = vec![site];
    cfg
}

/// 多站点 prod 语义：public 主机 / server_names 白名单放行，未知与不可解析 Host 421。
#[tokio::test]
async fn multisite_allows_public_and_server_names_but_rejects_unknown_host() {
    let site = prod_site(
        "app1",
        8081,
        &["app1-alt.example.com"],
        Some("https://app1.example.com"),
    );
    let state = make_state(prod_config(site));
    let bff = spawn_site(&state, "app1").await;
    let client = test_client();

    // public_base_url 主机（含端口/大小写变体）→ 放行
    for host in [
        "app1.example.com",
        "app1.example.com:443",
        "App1.Example.COM",
    ] {
        let resp = client
            .get(format!("{bff}/api/session"))
            .header("host", host)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200, "Host={host} 应放行");
    }

    // server_names 白名单主机 → 放行
    let resp = client
        .get(format!("{bff}/api/session"))
        .header("host", "app1-alt.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200, "server_names 白名单应放行");

    // 未知主机 → 421 + JSON error；且不得先于会话层建立会话
    let resp = client
        .get(format!("{bff}/api/session"))
        .header("host", "evil.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 421, "未知 Host 必须 421");
    assert!(
        !resp_headers_have_cookie(&resp),
        "421 必须在会话层之前执行，不得创建会话"
    );
    let body = resp.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["error"], "Misdirected Request");

    // 不可解析的 Host（空白）→ 421
    let resp = client
        .get(format!("{bff}/api/session"))
        .header("host", "bad host name")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 421, "不可解析的 Host 必须 421");
}

fn resp_headers_have_cookie(resp: &reqwest::Response) -> bool {
    resp.headers().contains_key("set-cookie")
}

/// `/live`、`/ready` 任意 Host 无条件豁免（探针来源是节点 IP）。
#[tokio::test]
async fn live_and_ready_are_exempt_from_host_validation() {
    let site = prod_site("app1", 8081, &[], Some("https://app1.example.com"));
    let state = make_state(prod_config(site));
    let bff = spawn_site(&state, "app1").await;
    let client = test_client();

    for path in ["/live", "/ready"] {
        for host in ["evil.example.com", "app1.example.com"] {
            let resp = client
                .get(format!("{bff}{path}"))
                .header("host", host)
                .send()
                .await
                .unwrap();
            assert_eq!(
                resp.status().as_u16(),
                200,
                "{path} Host={host} 应无条件豁免"
            );
        }
    }
}

/// prod 语义（配置了 public_base_url）下 loopback Host 也拒绝。
#[tokio::test]
async fn loopback_rejected_when_public_base_url_configured() {
    let site = prod_site("app1", 8081, &[], Some("https://app1.example.com"));
    let state = make_state(prod_config(site));
    let bff = spawn_site(&state, "app1").await;
    let client = test_client();

    for host in ["localhost", "localhost:8081", "127.0.0.1", "[::1]"] {
        let resp = client
            .get(format!("{bff}/api/session"))
            .header("host", host)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status().as_u16(),
            421,
            "Host={host} 在 prod 语义下必须拒绝"
        );
    }
}

/// legacy 未启用 enforce_host：行为冻结，业务路径任意 Host 照常（绝不 421）。
#[tokio::test]
async fn legacy_without_enforce_host_is_unchanged() {
    let mut cfg = base_config();
    cfg.spa.dir = common::make_spa_dir("legacy-host");
    let bff = spawn_business(make_state(cfg)).await;
    let client = test_client();

    // 业务路径（SPA fallback）伪造 Host → 200
    let resp = client
        .get(format!("{bff}/dashboard"))
        .header("host", "evil.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "legacy 无 enforce_host 不得 421"
    );

    // /api/session 同样不受 Host 校验
    let resp = client
        .get(format!("{bff}/api/session"))
        .header("host", "evil.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

/// legacy 启用 enforce_host：全部路径应用白名单（trusted_hosts ∪ public 主机）。
#[tokio::test]
async fn legacy_enforce_host_applies_allowlist() {
    let mut cfg = base_config();
    cfg.server.trusted_hosts = vec!["bff.internal".into()];
    cfg.server.enforce_host = true;
    let bff = spawn_business(make_state(cfg)).await;
    let client = test_client();

    // 白名单主机 → 放行
    let resp = client
        .get(format!("{bff}/api/session"))
        .header("host", "bff.internal")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200, "trusted_hosts 白名单应放行");

    // 未知主机 → 421
    let resp = client
        .get(format!("{bff}/api/session"))
        .header("host", "evil.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        421,
        "enforce_host 下未知 Host 必须 421"
    );
}

/// legacy enforce_host 且白名单为空：仅 loopback 放行。
#[tokio::test]
async fn legacy_enforce_host_empty_allowlist_loopback_only() {
    let mut cfg = base_config();
    cfg.server.enforce_host = true;
    let bff = spawn_business(make_state(cfg)).await;
    let client = test_client();

    let resp = client
        .get(format!("{bff}/api/session"))
        .header("host", "localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "白名单为空时 loopback Host 应放行"
    );

    let resp = client
        .get(format!("{bff}/api/session"))
        .header("host", "evil.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        421,
        "白名单为空时非 loopback Host 必须 421"
    );
}

/// dev 语义（未配置 `public_base_url`）的 loopback 兜底应基于**归一化后**的主机名：
/// 大小写与尾点差异不得把合法的开发用 loopback Host 判成 421。
#[tokio::test]
async fn dev_loopback_host_comparison_is_normalized() {
    let site = prod_site("app1", 8081, &[], None);
    let state = make_state(prod_config(site));
    let bff = spawn_site(&state, "app1").await;
    let client = test_client();

    for host in ["LOCALHOST", "localhost.", "Localhost:8081"] {
        let resp = client
            .get(format!("{bff}/api/session"))
            .header("host", host)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status().as_u16(),
            200,
            "dev loopback Host={host} 应放行（归一化比较）"
        );
    }
}

/// 探针路径的豁免应容忍尾斜杠：`/live/` 同样无条件跳过 Host 校验。
#[tokio::test]
async fn probe_path_with_trailing_slash_is_exempt() {
    let site = prod_site("app1", 8081, &[], Some("https://app1.example.com"));
    let state = make_state(prod_config(site));
    let bff = spawn_site(&state, "app1").await;
    let client = test_client();

    let resp = client
        .get(format!("{bff}/live/"))
        .header("host", "evil.example.com")
        .send()
        .await
        .unwrap();
    assert_ne!(
        resp.status().as_u16(),
        421,
        "/live/ 应与 /live 一样豁免 Host 校验"
    );
}

/// Host 头缺失（裸 TCP）→ 421。
#[tokio::test]
async fn missing_host_header_gets_421() {
    let site = prod_site("app1", 8081, &[], Some("https://app1.example.com"));
    let state = make_state(prod_config(site));
    let bff = spawn_site(&state, "app1").await;
    let addr: std::net::SocketAddr = bff
        .trim_start_matches("http://")
        .parse()
        .expect("spawn 返回 http://127.0.0.1:port");

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /api/session HTTP/1.1\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).await.unwrap();
    assert!(
        buf.starts_with("HTTP/1.1 421"),
        "缺少 Host 头的非豁免请求必须 421，实际响应: {buf:?}"
    );
}
