//! OIDC 客户端管理：按 (provider_id, base_url) 懒加载（discovery 为异步），结果缓存。
//! 管理端更新 provider 后调用 `invalidate` 使缓存失效。
//!
//! discovery 使用注入的带超时共享客户端（oauth2 5 起 `reqwest::Client` 直接实现
//! `AsyncHttpClient`，不再需要闭包包装；带超时/禁重定向的客户端见 `state.rs::build_http_client`）。
use crate::config::OidcProviderConfig;
use anyhow::Context;
use openidconnect::core::{CoreClient, CoreProviderMetadata};
use openidconnect::{
    ClientId, ClientSecret, EndpointMaybeSet, EndpointNotSet, EndpointSet, IssuerUrl, RedirectUrl,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// 缓存中的 OIDC 客户端类型（openidconnect 4.0 引入 typestate 泛型）。
///
/// `from_provider_metadata` 产出的 `HasTokenUrl` 为 `EndpointMaybeSet`（无法直接换码）；
/// `build_client` 用 discovery 元数据里的 `token_endpoint` 经 `set_token_uri` 升级为
/// `EndpointSet`——provider 缺失 token endpoint 时在构建期即报错（原实现推迟到换码时）。
pub type BffCoreClient = CoreClient<
    EndpointSet,      // HasAuthUrl：metadata 必含授权端点
    EndpointNotSet,   // HasDeviceAuthUrl
    EndpointNotSet,   // HasIntrospectionUrl
    EndpointNotSet,   // HasRevocationUrl
    EndpointSet,      // HasTokenUrl：build_client 升级
    EndpointMaybeSet, // HasUserInfoUrl
>;

pub struct OidcClientManager {
    clients: RwLock<HashMap<String, Arc<BffCoreClient>>>,
    /// discovery 元数据中的 `end_session_endpoint` 缓存（None = 已探测但不存在）
    logout_endpoints: RwLock<HashMap<String, Option<String>>>,
    /// OIDC 出网专用客户端（bounded 超时 + 连接池复用）
    http: reqwest::Client,
}

impl OidcClientManager {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
            logout_endpoints: RwLock::new(HashMap::new()),
            http,
        }
    }

    /// 发现 IdP 的 RP-Initiated Logout 端点（`end_session_endpoint`）。
    ///
    /// 各 IdP 登出路径不同（Spring AS `/connect/logout`、Keycloak
    /// `/protocol/openid-connect/logout`、Okta `/oauth2/v1/logout` …），
    /// 硬编码必然“换个 IdP 就登不出去”——一律以 discovery 元数据为准。
    pub async fn end_session_endpoint(&self, cfg: &OidcProviderConfig) -> Option<String> {
        if let Some(cached) = self.logout_endpoints.read().await.get(&cfg.id) {
            return cached.clone();
        }
        let issuer = match IssuerUrl::new(cfg.issuer_url.clone()) {
            Ok(i) => i,
            Err(_) => return None,
        };
        let endpoint =
            match openidconnect::ProviderMetadataWithLogout::discover_async(issuer, &self.http)
                .await
            {
                Ok(md) => md
                    .additional_metadata()
                    .end_session_endpoint
                    .as_ref()
                    .map(|u| u.url().to_string()),
                Err(e) => {
                    tracing::warn!(provider = %cfg.id, error = %e, "登出端点 discovery 失败");
                    None
                }
            };
        self.logout_endpoints
            .write()
            .await
            .insert(cfg.id.clone(), endpoint.clone());
        endpoint
    }

    /// 缓存键必须包含 base_url：redirect_uri 在 build_client 时烧入客户端，
    /// 若仅按 provider id 缓存，首个调用者的 base_url 会污染其余全部请求。
    fn cache_key(cfg: &OidcProviderConfig, base_url: &str) -> String {
        format!("{}|{}", cfg.id, base_url.trim_end_matches('/'))
    }

    /// 获取（或构建）provider 对应的 OIDC 客户端。
    pub async fn get(
        &self,
        cfg: &OidcProviderConfig,
        base_url: &str,
    ) -> anyhow::Result<Arc<BffCoreClient>> {
        let key = Self::cache_key(cfg, base_url);
        if let Some(c) = self.clients.read().await.get(&key) {
            return Ok(c.clone());
        }
        let mut w = self.clients.write().await;
        if let Some(c) = w.get(&key) {
            return Ok(c.clone());
        }
        let client = build_client(cfg, base_url, &self.http).await?;
        let client = Arc::new(client);
        w.insert(key, client.clone());
        Ok(client)
    }

    /// 失效某 provider 的**全部**客户端（所有 base_url 变体）与登出端点缓存。
    pub async fn invalidate(&self, provider_id: &str) {
        let prefix = format!("{}|", provider_id);
        self.clients
            .write()
            .await
            .retain(|k, _| !k.starts_with(&prefix));
        self.logout_endpoints.write().await.remove(provider_id);
    }
}

async fn build_client(
    cfg: &OidcProviderConfig,
    base_url: &str,
    http: &reqwest::Client,
) -> anyhow::Result<BffCoreClient> {
    let issuer = IssuerUrl::new(cfg.issuer_url.clone()).context("issuer_url 非法")?;
    let metadata = CoreProviderMetadata::discover_async(issuer, http)
        .await
        .with_context(|| format!("OIDC discovery 失败: {}", cfg.issuer_url))?;
    // 4.0 typestate：token endpoint 需显式“升级”后才能换码/刷新；
    // 缺失时在构建期报错（快速失败，避免运行期才发现 provider 契约不完整）。
    let token_uri = metadata.token_endpoint().cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "provider discovery 未提供 token_endpoint: {}",
            cfg.issuer_url
        )
    })?;
    let redirect = RedirectUrl::new(format!(
        "{}{}",
        base_url.trim_end_matches('/'),
        cfg.callback_path
    ))
    .context("redirect_uri 非法")?;
    let secret = if cfg.client_secret.is_empty() {
        None
    } else {
        Some(ClientSecret::new(cfg.client_secret.clone()))
    };
    let client =
        CoreClient::from_provider_metadata(metadata, ClientId::new(cfg.client_id.clone()), secret)
            .set_redirect_uri(redirect)
            .set_token_uri(token_uri);
    Ok(client)
}
