//! OIDC 客户端管理：按 provider 懒加载（discovery 为异步），结果缓存。
//! 管理端更新 provider 后调用 `invalidate` 使缓存失效。
use crate::config::OidcProviderConfig;
use anyhow::Context;
use openidconnect::core::{CoreClient, CoreProviderMetadata};
use openidconnect::{ClientId, ClientSecret, IssuerUrl, RedirectUrl};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct OidcClientManager {
    clients: RwLock<HashMap<String, Arc<CoreClient>>>,
}

impl Default for OidcClientManager {
    fn default() -> Self {
        Self::new()
    }
}

impl OidcClientManager {
    pub fn new() -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
        }
    }

    /// 获取（或构建）provider 对应的 OIDC 客户端。
    pub async fn get(
        &self,
        cfg: &OidcProviderConfig,
        base_url: &str,
    ) -> anyhow::Result<Arc<CoreClient>> {
        if let Some(c) = self.clients.read().await.get(&cfg.id) {
            return Ok(c.clone());
        }
        let mut w = self.clients.write().await;
        if let Some(c) = w.get(&cfg.id) {
            return Ok(c.clone());
        }
        let client = build_client(cfg, base_url).await?;
        let client = Arc::new(client);
        w.insert(cfg.id.clone(), client.clone());
        Ok(client)
    }

    pub async fn invalidate(&self, provider_id: &str) {
        self.clients.write().await.remove(provider_id);
    }
}

async fn build_client(cfg: &OidcProviderConfig, base_url: &str) -> anyhow::Result<CoreClient> {
    let issuer = IssuerUrl::new(cfg.issuer_url.clone()).context("issuer_url 非法")?;
    let metadata =
        CoreProviderMetadata::discover_async(issuer, openidconnect::reqwest::async_http_client)
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
