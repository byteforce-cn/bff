//! 站点运行时类型：`SiteHandle`（启动期静态）/ `SiteView`（每请求动态）/ `SiteCtx`、
//! 安全响应头预构建，以及站点级令牌解析（§6.1 / §7.2 / §9）。
//!
//! 本模块不依赖 `AppState`（避免循环依赖）；会话读写走 `tower_sessions::Session`。

use crate::config::{
    LogoutScope, ResolvedSite, SecurityHeadersConfig, SiteSecurityHeadersOverride,
};
use crate::oidc::tokens::{session_key, StoredTokens};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use std::sync::Arc;
use tower_sessions::session::Session;
use tower_sessions::SessionManagerLayer;

/// legacy 兼容的全局 provider 键（仅 legacy 模式回退读取并迁移，§7.2）。
pub const LEGACY_CURRENT_PROVIDER_KEY: &str = "oidc:current_provider";

/// 站点运行时句柄（静态，启动时构建）：监听与 session 层，注入站点 router。
#[derive(Debug, Clone)]
pub struct SiteHandle {
    pub name: String,
    pub port: u16,
    pub bind: String,
    /// 引用的 session profile 名（与 `session_layer` 一一对应）
    pub session_profile: String,
    pub session_layer: SessionManagerLayer<crate::provider::session::DynSessionStore>,
    pub legacy: bool,
}

/// 站点配置视图（动态，每请求从配置快照按站点名解析）。
///
/// 安全响应头为预构建值（`HeaderValue` 已解析），请求路径零解析成本（§9）。
pub struct SiteView {
    pub name: String,
    pub port: u16,
    pub server_names: Vec<String>,
    pub allowed_hosts: Vec<String>,
    pub public_base_url: Option<String>,
    pub spa_dir: String,
    pub default_provider: String,
    pub allowed_providers: Vec<String>,
    pub logout_scope: LogoutScope,
    pub security_headers: Arc<PrebuiltSecurityHeaders>,
    pub legacy: bool,
}

/// 显式站点上下文：沿 `dispatch → proxy / token_exchange / pipeline / ws` 传参。
pub struct SiteCtx<'a> {
    pub handle: &'a SiteHandle,
    pub view: Arc<SiteView>,
}

/// 预构建的安全响应头（`HeaderValue` 已解析；`None` = 不发送该头）。
pub struct PrebuiltSecurityHeaders {
    /// 全局 CSP（站点未整体替换时生效；`None` = 不发送）
    pub csp_default: Option<HeaderValue>,
    /// 按路径前缀的 CSP 覆盖（按前缀长度降序；站点整体替换 CSP 时为空）
    pub csp_overrides: Vec<(String, HeaderValue)>,
    pub x_frame_options: Option<HeaderValue>,
    pub x_content_type_options: Option<HeaderValue>,
    /// Strict-Transport-Security（已预构建为 `max-age=N`）
    pub hsts: Option<HeaderValue>,
    pub referrer_policy: Option<HeaderValue>,
}

impl PrebuiltSecurityHeaders {
    /// 合并全局安全头配置与站点覆盖（§9）：
    /// - `ov.content_security_policy` 为 `Some` 时整体替换全局 CSP，`csp_overrides` 清空；
    /// - 其余字段逐字段 `unwrap_or(global)`；
    /// - 空串字段视为“不发送”（`None`）；
    /// - `HeaderValue` 解析失败返回 `Err`（绝不 panic）。
    pub fn build(
        global: &SecurityHeadersConfig,
        ov: Option<&SiteSecurityHeadersOverride>,
    ) -> anyhow::Result<Self> {
        // 空串 = 不发送；非法字符 = Err
        fn parse_opt(raw: &str) -> anyhow::Result<Option<HeaderValue>> {
            if raw.is_empty() {
                return Ok(None);
            }
            HeaderValue::from_str(raw)
                .map(Some)
                .map_err(|e| anyhow::anyhow!("安全响应头非法: {}", e))
        }

        let replace_csp = ov.is_some_and(|o| o.content_security_policy.is_some());
        let csp_default = match ov.and_then(|o| o.content_security_policy.as_deref()) {
            Some(raw) => parse_opt(raw)?,
            None => parse_opt(&global.content_security_policy)?,
        };
        let csp_overrides = if replace_csp {
            // 站点 CSP 一旦指定即整体替换（含全局 path-prefix 覆盖）
            vec![]
        } else {
            // 继承全局覆盖；空 CSP 条目跳过（该前缀回退全局 CSP）；
            // 按前缀长度降序预解析，apply 时取首个命中
            let mut parsed = Vec::with_capacity(global.csp_overrides.len());
            for entry in &global.csp_overrides {
                if entry.content_security_policy.is_empty() {
                    continue;
                }
                let value = HeaderValue::from_str(&entry.content_security_policy).map_err(|e| {
                    anyhow::anyhow!("csp_overrides[{}] 非法: {}", entry.path_prefix, e)
                })?;
                parsed.push((entry.path_prefix.clone(), value));
            }
            parsed.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
            parsed
        };

        let x_frame_options = parse_opt(
            ov.and_then(|o| o.x_frame_options.as_deref())
                .unwrap_or(&global.x_frame_options),
        )?;
        let x_content_type_options = parse_opt(
            ov.and_then(|o| o.x_content_type_options.as_deref())
                .unwrap_or(&global.x_content_type_options),
        )?;
        let referrer_policy = parse_opt(
            ov.and_then(|o| o.referrer_policy.as_deref())
                .unwrap_or(&global.referrer_policy),
        )?;
        let hsts = {
            let max_age = ov
                .and_then(|o| o.hsts_max_age)
                .unwrap_or(global.hsts_max_age);
            if max_age == 0 {
                None // 0 = 不发送
            } else {
                Some(
                    HeaderValue::from_str(&format!("max-age={}", max_age))
                        .map_err(|e| anyhow::anyhow!("hsts 头非法: {}", e))?,
                )
            }
        };

        Ok(Self {
            csp_default,
            csp_overrides,
            x_frame_options,
            x_content_type_options,
            hsts,
            referrer_policy,
        })
    }

    /// 请求路径选 CSP：最长前缀命中优先，未命中回退全局（与 legacy 行为一致）。
    fn csp_for(&self, path: &str) -> Option<&HeaderValue> {
        self.csp_overrides
            .iter()
            .find(|(prefix, _)| path.starts_with(prefix.as_str()))
            .map(|(_, value)| value)
            .or(self.csp_default.as_ref())
    }

    /// 把预构建值写入响应头：零字符串解析，`None` 跳过（§9 按请求读预构建值）。
    pub fn apply(&self, path: &str, headers: &mut HeaderMap) {
        if let Some(csp) = self.csp_for(path) {
            headers.insert(
                HeaderName::from_static("content-security-policy"),
                csp.clone(),
            );
        }
        if let Some(value) = &self.x_frame_options {
            headers.insert(HeaderName::from_static("x-frame-options"), value.clone());
        }
        if let Some(value) = &self.x_content_type_options {
            headers.insert(
                HeaderName::from_static("x-content-type-options"),
                value.clone(),
            );
        }
        if let Some(value) = &self.hsts {
            headers.insert(
                HeaderName::from_static("strict-transport-security"),
                value.clone(),
            );
        }
        if let Some(value) = &self.referrer_policy {
            headers.insert(HeaderName::from_static("referrer-policy"), value.clone());
        }
    }
}

impl SiteView {
    /// 从解析后的站点 + 预构建安全头构造视图（§6.1 每请求解析）。
    pub fn from_resolved(
        site: &ResolvedSite,
        security_headers: Arc<PrebuiltSecurityHeaders>,
    ) -> Self {
        Self {
            name: site.name.clone(),
            port: site.port,
            server_names: site.server_names.clone(),
            allowed_hosts: site.allowed_hosts.clone(),
            public_base_url: site.public_base_url.clone(),
            spa_dir: site.spa_dir.clone(),
            default_provider: site.default_provider.clone(),
            allowed_providers: site.allowed_providers.clone(),
            logout_scope: site.logout_scope,
            security_headers,
            legacy: site.legacy,
        }
    }

    /// 站点维度 provider 键（§7.2）：`oidc:{site}:current_provider`。
    pub fn provider_key(&self) -> String {
        format!("oidc:{}:current_provider", self.name)
    }

    /// 该 provider 是否在站点白名单内。
    fn allowed(&self, provider: &str) -> bool {
        self.allowed_providers.iter().any(|p| p == provider)
    }

    /// provider 是否有 token 存于会话。
    async fn has_tokens(&self, session: &Session, provider: &str) -> bool {
        session
            .get::<StoredTokens>(&session_key(provider))
            .await
            .ok()
            .flatten()
            .is_some()
    }

    /// 站点当前 provider 统一处理逻辑（§7.2）：
    /// 1. 读 `oidc:{site}:current_provider`：有效、∈ 白名单且对应 token 存在 → 返回；
    /// 2. legacy 模式回退读旧键 `oidc:current_provider`：有效、∈ 白名单且有 token →
    ///    迁移（写站点键、删旧键）后返回。必须在 default 回退之前读取：旧键命中的
    ///    provider 应保持升级前语义，不能因 default 有 token 而改选（行为冻结）；
    /// 3. 否则尝试站点 `default_provider` 的 token，有则写回站点键并返回；
    /// 4. 仍无 → `None`（该站点未登录）。显式多站点模式永不读旧键。
    pub async fn current_provider(&self, session: &Session) -> Option<String> {
        // 1. 站点键
        if let Some(provider) = session
            .get::<String>(&self.provider_key())
            .await
            .ok()
            .flatten()
        {
            if self.allowed(&provider) && self.has_tokens(session, &provider).await {
                return Some(provider);
            }
        }

        // 2. legacy 旧键回退 + 迁移（仅 legacy 模式）
        if self.legacy {
            if let Some(provider) = session
                .get::<String>(LEGACY_CURRENT_PROVIDER_KEY)
                .await
                .ok()
                .flatten()
            {
                if self.allowed(&provider) && self.has_tokens(session, &provider).await {
                    session.insert(&self.provider_key(), &provider).await.ok();
                    session.remove_value(LEGACY_CURRENT_PROVIDER_KEY).await.ok();
                    return Some(provider);
                }
            }
        }

        // 3. default_provider 回退 + 写回
        if !self.default_provider.is_empty()
            && self.allowed(&self.default_provider)
            && self.has_tokens(session, &self.default_provider).await
        {
            session
                .insert(&self.provider_key(), &self.default_provider)
                .await
                .ok();
            return Some(self.default_provider.clone());
        }

        None
    }

    /// 站点当前 provider 的令牌（无则视为该站点未登录）。
    pub async fn current_tokens(&self, session: &Session) -> Option<StoredTokens> {
        let provider = self.current_provider(session).await?;
        session.get(&session_key(&provider)).await.ok().flatten()
    }

    /// 站点当前明文 access token（供代理层/中间件使用）。
    pub async fn current_access_token(&self, session: &Session) -> Option<String> {
        self.current_tokens(session).await?.access_token().ok()
    }

    /// 站点规范基础 URL（§8.2）：优先 `public_base_url`，dev 回退 loopback + 站点端口。
    pub fn canonical_base_url(&self) -> String {
        self.public_base_url
            .clone()
            .unwrap_or_else(|| format!("http://127.0.0.1:{}", self.port))
            .trim_end_matches('/')
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        CspOverrideConfig, LogoutScope, ResolvedSite, SecurityHeadersConfig,
        SiteSecurityHeadersOverride,
    };
    use crate::oidc::tokens::{session_key, StoredTokens};
    use crate::provider::session::DynSessionStore;
    use axum::http::HeaderMap;
    use std::sync::Arc;
    use tower_sessions::session_store::SessionStore;
    use tower_sessions::{MemoryStore, Session};

    /// 测试用站点（真实 ResolvedSite 由 AppConfig::effective_sites 合成）。
    fn site(name: &str, default_provider: &str, allowed: &[&str], legacy: bool) -> ResolvedSite {
        ResolvedSite {
            name: name.into(),
            port: 8080,
            bind: "0.0.0.0".into(),
            server_names: vec![],
            public_base_url: None,
            public_host: None,
            allowed_hosts: vec![],
            spa_dir: "dist".into(),
            session_profile: "default".into(),
            default_provider: default_provider.into(),
            allowed_providers: allowed.iter().map(|s| s.to_string()).collect(),
            logout_scope: LogoutScope::Global,
            security_headers: None,
            legacy,
        }
    }

    /// 进程级一次性初始化加密（StoredTokens::new 依赖）。
    fn init_crypto() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            crate::utils::crypto::init("test-secret", "test-salt-0123456789").unwrap();
        });
    }

    fn tokens(provider: &str) -> StoredTokens {
        init_crypto();
        StoredTokens::new(provider, "sub-1", "access-token", None, None, 3600).unwrap()
    }

    fn store() -> Arc<dyn SessionStore> {
        Arc::new(MemoryStore::default())
    }

    fn new_session() -> Session {
        Session::new(None, Arc::new(DynSessionStore::new(store())), None)
    }

    fn headers() -> Arc<PrebuiltSecurityHeaders> {
        Arc::new(PrebuiltSecurityHeaders::build(&SecurityHeadersConfig::default(), None).unwrap())
    }

    fn view_with_allowed(allowed: &[&str], default: &str) -> Arc<SiteView> {
        Arc::new(SiteView::from_resolved(
            &site("test-site", default, allowed, false),
            headers(),
        ))
    }

    fn legacy_view(allowed: &[&str], default: &str) -> Arc<SiteView> {
        Arc::new(SiteView::from_resolved(
            &site("default", default, allowed, true),
            headers(),
        ))
    }

    #[tokio::test]
    async fn current_provider_falls_back_and_writes_back() {
        let view = view_with_allowed(&["p1", "p2"], "p2");
        let session = new_session();
        session
            .insert(&session_key("p2"), tokens("p2"))
            .await
            .unwrap();
        assert_eq!(view.current_provider(&session).await.as_deref(), Some("p2"));
        let written: Option<String> = session.get(&view.provider_key()).await.unwrap();
        assert_eq!(written.as_deref(), Some("p2"));
    }

    #[tokio::test]
    async fn stale_provider_without_tokens_falls_back_to_default() {
        let view = view_with_allowed(&["p1", "p2"], "p2");
        let session = new_session();
        // 站点键指向 p1，但 p1 无 token；default p2 有 token → 回退并写回
        session.insert(&view.provider_key(), "p1").await.unwrap();
        session
            .insert(&session_key("p2"), tokens("p2"))
            .await
            .unwrap();
        assert_eq!(view.current_provider(&session).await.as_deref(), Some("p2"));
        let written: Option<String> = session.get(&view.provider_key()).await.unwrap();
        assert_eq!(written.as_deref(), Some("p2"));
    }

    #[tokio::test]
    async fn provider_outside_whitelist_is_ignored() {
        let view = view_with_allowed(&["p1"], "p1");
        let session = new_session();
        // 白名单外 provider p9 的键与 token：均不得被采用
        session.insert(&view.provider_key(), "p9").await.unwrap();
        session
            .insert(&session_key("p9"), tokens("p9"))
            .await
            .unwrap();
        assert!(view.current_tokens(&session).await.is_none());
        // 不删除、不写回任何值
        let written: Option<String> = session.get(&view.provider_key()).await.unwrap();
        assert_eq!(written.as_deref(), Some("p9"));
    }

    #[tokio::test]
    async fn legacy_key_is_migrated_only_in_legacy_mode() {
        // legacy=true：读旧键、迁移为站点键并删除旧键（旧键 provider ≠ default，
        // 且 default 无 token —— 确保走旧键回退而非 default 回退）
        let legacy = legacy_view(&["p1", "p2"], "p2");
        let session = new_session();
        session.insert("oidc:current_provider", "p1").await.unwrap();
        session
            .insert(&session_key("p1"), tokens("p1"))
            .await
            .unwrap();
        assert_eq!(
            legacy.current_provider(&session).await.as_deref(),
            Some("p1")
        );
        let old: Option<String> = session.get("oidc:current_provider").await.unwrap();
        assert!(old.is_none(), "旧键应被删除");
        let new: Option<String> = session.get(&legacy.provider_key()).await.unwrap();
        assert_eq!(new.as_deref(), Some("p1"));

        // legacy=true 且旧键 provider == default：同样完成迁移并删除旧键
        let legacy_same = legacy_view(&["p1"], "p1");
        let session = new_session();
        session.insert("oidc:current_provider", "p1").await.unwrap();
        session
            .insert(&session_key("p1"), tokens("p1"))
            .await
            .unwrap();
        assert_eq!(
            legacy_same.current_provider(&session).await.as_deref(),
            Some("p1")
        );
        let old: Option<String> = session.get("oidc:current_provider").await.unwrap();
        assert!(old.is_none(), "旧键应被删除");

        // legacy=false：同样输入 → 不读旧键；default p2 无 token → None
        let explicit = view_with_allowed(&["p1", "p2"], "p2");
        let session2 = new_session();
        session2
            .insert("oidc:current_provider", "p1")
            .await
            .unwrap();
        session2
            .insert(&session_key("p1"), tokens("p1"))
            .await
            .unwrap();
        assert!(explicit.current_provider(&session2).await.is_none());
        let old: Option<String> = session2.get("oidc:current_provider").await.unwrap();
        assert_eq!(old.as_deref(), Some("p1"), "旧键不应被触碰");
        let new: Option<String> = session2.get(&explicit.provider_key()).await.unwrap();
        assert!(new.is_none());
    }

    #[tokio::test]
    async fn current_tokens_uses_site_provider_key() {
        let view = view_with_allowed(&["p1", "p2"], "p2");
        let session = new_session();
        session.insert(&view.provider_key(), "p1").await.unwrap();
        session
            .insert(&session_key("p1"), tokens("p1"))
            .await
            .unwrap();
        let got = view.current_tokens(&session).await.unwrap();
        assert_eq!(got.provider, "p1");
        assert_eq!(
            view.current_access_token(&session).await.as_deref(),
            Some("access-token")
        );
    }

    #[test]
    fn prebuilt_headers_csp_replaces_overrides_only_when_specified() {
        let global = SecurityHeadersConfig {
            csp_overrides: vec![CspOverrideConfig {
                path_prefix: "/admin".into(),
                content_security_policy: "default-src 'self'".into(),
            }],
            ..Default::default()
        };
        let inherit = PrebuiltSecurityHeaders::build(&global, None).unwrap();
        assert_eq!(inherit.csp_overrides.len(), 1);
        let ov = SiteSecurityHeadersOverride {
            content_security_policy: Some("default-src 'none'".into()),
            ..Default::default()
        };
        let replaced = PrebuiltSecurityHeaders::build(&global, Some(&ov)).unwrap();
        assert!(replaced.csp_overrides.is_empty());
        assert_eq!(
            replaced.csp_default.unwrap().to_str().unwrap(),
            "default-src 'none'"
        );
    }

    #[test]
    fn prebuilt_headers_reject_invalid_header_values() {
        let global = SecurityHeadersConfig::default();
        let ov = SiteSecurityHeadersOverride {
            content_security_policy: Some("default-src 'none'\nX-Evil: 1".into()),
            ..Default::default()
        };
        assert!(PrebuiltSecurityHeaders::build(&global, Some(&ov)).is_err());
        let global2 = SecurityHeadersConfig {
            csp_overrides: vec![CspOverrideConfig {
                path_prefix: "/x".into(),
                content_security_policy: "a\nb".into(),
            }],
            ..Default::default()
        };
        assert!(PrebuiltSecurityHeaders::build(&global2, None).is_err());
    }

    #[test]
    fn prebuilt_headers_empty_strings_mean_not_sent() {
        let global = SecurityHeadersConfig::default();
        // 空串 CSP = 整体替换为不发送；空串 x_frame_options = 不发送
        let ov = SiteSecurityHeadersOverride {
            content_security_policy: Some("".into()),
            x_frame_options: Some("".into()),
            ..Default::default()
        };
        let built = PrebuiltSecurityHeaders::build(&global, Some(&ov)).unwrap();
        assert!(built.csp_default.is_none());
        assert!(built.csp_overrides.is_empty());
        assert!(built.x_frame_options.is_none());
        let mut headers = HeaderMap::new();
        built.apply("/", &mut headers);
        assert!(headers.get("content-security-policy").is_none());
        assert!(headers.get("x-frame-options").is_none());
        // 未覆盖字段仍继承全局
        assert_eq!(
            headers
                .get("x-content-type-options")
                .unwrap()
                .to_str()
                .unwrap(),
            "nosniff"
        );
    }

    #[test]
    fn prebuilt_headers_hsts_prebuilt_as_max_age() {
        let global = SecurityHeadersConfig {
            hsts_max_age: 31536000,
            ..Default::default()
        };
        let built = PrebuiltSecurityHeaders::build(&global, None).unwrap();
        assert_eq!(
            built.hsts.as_ref().unwrap().to_str().unwrap(),
            "max-age=31536000"
        );
        // 覆盖为 0 → 不发送
        let ov = SiteSecurityHeadersOverride {
            hsts_max_age: Some(0),
            ..Default::default()
        };
        let built2 = PrebuiltSecurityHeaders::build(&global, Some(&ov)).unwrap();
        assert!(built2.hsts.is_none());
    }

    #[test]
    fn apply_selects_longest_prefix_and_default() {
        let global = SecurityHeadersConfig {
            content_security_policy: "default-src 'self'".into(),
            csp_overrides: vec![
                CspOverrideConfig {
                    path_prefix: "/a".into(),
                    content_security_policy: "csp-a".into(),
                },
                CspOverrideConfig {
                    path_prefix: "/a/b".into(),
                    content_security_policy: "csp-ab".into(),
                },
            ],
            ..Default::default()
        };
        let built = PrebuiltSecurityHeaders::build(&global, None).unwrap();
        let mut headers = HeaderMap::new();
        built.apply("/a/b/c", &mut headers);
        assert_eq!(
            headers
                .get("content-security-policy")
                .unwrap()
                .to_str()
                .unwrap(),
            "csp-ab"
        );
        headers.clear();
        built.apply("/a/x", &mut headers);
        assert_eq!(
            headers
                .get("content-security-policy")
                .unwrap()
                .to_str()
                .unwrap(),
            "csp-a"
        );
        headers.clear();
        built.apply("/other", &mut headers);
        assert_eq!(
            headers
                .get("content-security-policy")
                .unwrap()
                .to_str()
                .unwrap(),
            "default-src 'self'"
        );
        assert!(headers.get("strict-transport-security").is_none());
    }

    #[test]
    fn canonical_base_url_uses_public_url_or_loopback_port() {
        let resolved = ResolvedSite {
            public_base_url: Some("https://app1.example.com/".into()),
            ..site("app1", "p1", &["p1"], false)
        };
        let view = SiteView::from_resolved(&resolved, headers());
        assert_eq!(view.canonical_base_url(), "https://app1.example.com");

        let dev = ResolvedSite {
            public_base_url: None,
            port: 8082,
            ..site("app2", "p2", &["p2"], false)
        };
        let view2 = SiteView::from_resolved(&dev, headers());
        assert_eq!(view2.canonical_base_url(), "http://127.0.0.1:8082");
    }
}
