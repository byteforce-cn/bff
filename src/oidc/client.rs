//! OIDC 客户端管理：按 (provider_id, base_url) 懒加载（discovery 为异步），结果缓存。
//! 管理端更新 provider 后调用 `invalidate` 使缓存失效。
//!
//! R13：discovery 使用注入的带超时共享客户端（不再是每次新建、无超时的默认实现）。
use crate::config::OidcProviderConfig;
use anyhow::Context;
use openidconnect::core::{CoreClient, CoreProviderMetadata};
use openidconnect::{ClientId, ClientSecret, IssuerUrl, RedirectUrl};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct OidcClientManager {
    clients: RwLock<HashMap<String, Arc<CoreClient>>>,
    /// R13：OIDC 出网专用客户端（bounded 超时 + 连接池复用）
    http: reqwest::Client,
}

impl OidcClientManager {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
            http,
        }
    }

    /// 缓存键必须包含 base_url（P0-2）：redirect_uri 在 build_client 时烧入客户端，
    /// 若仅按 provider id 缓存，首个调用者的 base_url 会污染其余全部请求。
    fn cache_key(cfg: &OidcProviderConfig, base_url: &str) -> String {
        format!("{}|{}", cfg.id, base_url.trim_end_matches('/'))
    }

    /// 获取（或构建）provider 对应的 OIDC 客户端。
    pub async fn get(
        &self,
        cfg: &OidcProviderConfig,
        base_url: &str,
    ) -> anyhow::Result<Arc<CoreClient>> {
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

    /// 失效某 provider 的**全部**客户端（所有 base_url 变体）。
    pub async fn invalidate(&self, provider_id: &str) {
        let prefix = format!("{}|", provider_id);
        self.clients
            .write()
            .await
            .retain(|k, _| !k.starts_with(&prefix));
    }
}

async fn build_client(
    cfg: &OidcProviderConfig,
    base_url: &str,
    http: &reqwest::Client,
) -> anyhow::Result<CoreClient> {
    let issuer = IssuerUrl::new(cfg.issuer_url.clone()).context("issuer_url 非法")?;
    let metadata = CoreProviderMetadata::discover_async(
        issuer,
        crate::oidc::http_client::client_fn(http.clone()),
    )
    .await
    .with_context(|| format!("OIDC discovery 失败: {}", cfg.issuer_url))?;
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
            .set_redirect_uri(redirect);
    Ok(client)
}
