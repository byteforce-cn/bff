//! Session 中存储的令牌结构（敏感字段 AES-GCM 加密）。
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTokens {
    /// 加密后的 access token
    pub access_token_enc: String,
    /// 加密后的 refresh token（如有）
    pub refresh_token_enc: Option<String>,
    /// 加密后的 id token（如有）
    pub id_token_enc: Option<String>,
    /// access token 过期时间（unix 秒）
    pub expires_at: i64,
    pub sub: String,
    pub provider: String,
}

impl StoredTokens {
    pub fn new(
        provider: &str,
        sub: &str,
        access_token: &str,
        refresh_token: Option<&str>,
        id_token: Option<&str>,
        expires_in_secs: i64,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            access_token_enc: crate::utils::crypto::encrypt(access_token.as_bytes())?,
            refresh_token_enc: refresh_token
                .map(|t| crate::utils::crypto::encrypt(t.as_bytes()))
                .transpose()?,
            id_token_enc: id_token
                .map(|t| crate::utils::crypto::encrypt(t.as_bytes()))
                .transpose()?,
            expires_at: now_unix() + expires_in_secs,
            sub: sub.to_string(),
            provider: provider.to_string(),
        })
    }

    pub fn access_token(&self) -> anyhow::Result<String> {
        Ok(String::from_utf8(crate::utils::crypto::decrypt(
            &self.access_token_enc,
        )?)?)
    }

    pub fn refresh_token(&self) -> anyhow::Result<Option<String>> {
        self.refresh_token_enc
            .as_deref()
            .map(crate::utils::crypto::decrypt)
            .transpose()
            .map(|opt| opt.and_then(|b| String::from_utf8(b).ok()))
    }

    pub fn id_token(&self) -> anyhow::Result<Option<String>> {
        self.id_token_enc
            .as_deref()
            .map(crate::utils::crypto::decrypt)
            .transpose()
            .map(|opt| opt.and_then(|b| String::from_utf8(b).ok()))
    }

    /// 是否即将过期（含刷新余量）。
    pub fn is_expiring(&self, skew_secs: u64) -> bool {
        self.expires_at - skew_secs as i64 <= now_unix()
    }
}

pub fn session_key(provider_id: &str) -> String {
    format!("oidc:{}:tokens", provider_id)
}

pub fn flow_key(provider_id: &str) -> String {
    format!("oidc:{}:flow", provider_id)
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
