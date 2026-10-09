//! 配置定义与加载（figment 多层合并）。
//!
//! 合并优先级（低 → 高）：
//! 1. `config/base.yaml`
//! 2. `config/oidc/providers.yaml`
//! 3. `config/pipelines/*.yaml`（每个文件顶层 map 合并到 `pipelines` 键下）
//! 4. `config/routes/routes.yaml`
//! 5. `config/env/{BFF_ENV}.yaml`
//! 6. 环境变量 `BFF_` 前缀（`__` 分隔层级）——最高优先级（12-factor）

use figment::providers::{Env, Format, Serialized, Yaml};
use figment::Figment;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ── ${ENV:default} 表达式解析 ──

/// 解析 `${ENV_VAR:default}` 格式的字符串：
/// - `${VAR:fallback}` → 优先 `env VAR`，未设置取 `fallback`
/// - `${VAR}`          → 优先 `env VAR`，未设置保留原样
/// - 普通字符串         → 直接返回
fn resolve_env_or_default(raw: &str) -> String {
    if raw.starts_with("${") && raw.ends_with('}') {
        let inner = &raw[2..raw.len() - 1];
        if let Some((var, default)) = inner.split_once(':') {
            std::env::var(var).unwrap_or_else(|_| default.to_string())
        } else {
            std::env::var(inner).unwrap_or_else(|_| raw.to_string())
        }
    } else {
        raw.to_string()
    }
}

fn deserialize_env_or_default<'de, D>(d: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(d)?;
    Ok(resolve_env_or_default(&raw))
}

// ── 加密密钥配置 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BffSecretConfig {
    /// 加密主密钥，支持 `${BFF_SECRET:default}` 表达式
    #[serde(
        deserialize_with = "deserialize_env_or_default",
        default = "default_bff_secret"
    )]
    pub secret: String,
    /// Argon2id 盐值，支持 `${BFF_SECRET_SALT:default}` 表达式
    #[serde(
        deserialize_with = "deserialize_env_or_default",
        default = "default_bff_salt"
    )]
    pub salt: String,
}

impl Default for BffSecretConfig {
    fn default() -> Self {
        Self {
            secret: default_bff_secret(),
            salt: default_bff_salt(),
        }
    }
}

fn default_bff_secret() -> String {
    resolve_env_or_default("${BFF_SECRET:change-me-in-production}")
}

fn default_bff_salt() -> String {
    resolve_env_or_default("${BFF_SECRET_SALT:default-salt-at-least-16-bytes}")
}

// ── AppConfig ──

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub provider: ProviderConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub admin: AdminConfig,
    #[serde(default)]
    pub spa: SpaConfig,
    #[serde(default)]
    pub oidc: OidcSection,
    #[serde(default)]
    pub token_refresh: TokenRefreshConfig,
    /// 加密密钥配置（AES-256-GCM / Argon2id）
    #[serde(default)]
    pub bff_secret: BffSecretConfig,
    /// HTTP 客户端配置（连接池、超时等）
    #[serde(default)]
    pub http_client: HttpClientConfig,
    /// 全局限流配置
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    /// 认证端点 per-IP 限流（网络层纵深防御，默认关闭）
    #[serde(default)]
    pub auth_rate_limit: AuthRateLimitConfig,
    /// CORS 跨域配置
    #[serde(default)]
    pub cors: CorsConfig,
    /// 安全响应头配置
    #[serde(default)]
    pub security_headers: SecurityHeadersConfig,
    /// 请求体大小限制
    #[serde(default)]
    pub body_limit: BodyLimitConfig,
    /// 熔断器配置
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,
    /// 脚本引擎配置
    #[serde(default)]
    pub scripting: ScriptingConfig,
    /// WebSocket 隧道配置（超时/心跳/消息上限）
    #[serde(default)]
    pub websocket: WebSocketTunnelConfig,
    /// OpenTelemetry 追踪导出（默认禁用，仅保留 W3C traceparent 传播）
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    /// 配置持久化（管理端变更落盘 + 外部变更热重载）
    #[serde(default)]
    pub persistence: PersistenceConfig,
    /// 健康检查配置（就绪探针 / 存活探针）
    #[serde(default)]
    pub health: HealthConfig,
    #[serde(default)]
    pub pipelines: HashMap<String, PipelineDef>,
    #[serde(default)]
    pub routes: Vec<RouteDef>,
    /// 显式多站点定义（空 = legacy 单站点，按 §5.5 合成 `default` 站点）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<SiteConfig>,
    /// 额外会话 profile（`default` 由顶层 `session` 定义，不得在此重复声明）
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub session_profiles: HashMap<String, SessionProfileOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_business_port")]
    pub business_port: u16,
    #[serde(default = "default_admin_port")]
    pub admin_port: u16,
    /// 对外基础 URL（如 `https://bff.example.com`）。
    /// 设置后 OIDC `redirect_uri` / `post_logout_redirect_uri` 一律基于它推导，
    /// **不再信任 Host 头**（防止匿名 Host 污染全体用户的授权地址）。
    #[serde(default)]
    pub public_base_url: Option<String>,
    /// 可信 Host 白名单（未配置 `public_base_url` 时的回退路径防护）。
    /// 非空时：Host 必须命中白名单，否则拒绝；为空时仅允许 loopback Host。
    #[serde(default)]
    pub trusted_hosts: Vec<String>,
    /// 是否对全部业务路径强制 Host 白名单校验（legacy opt-in；显式多站点始终启用）
    #[serde(default, skip_serializing_if = "is_false")]
    pub enforce_host: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            business_port: default_business_port(),
            admin_port: default_admin_port(),
            public_base_url: None,
            trusted_hosts: Vec::new(),
            enforce_host: false,
        }
    }
}

fn default_business_port() -> u16 {
    8080
}
fn default_admin_port() -> u16 {
    8443
}

// ── HTTP 客户端配置 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpClientConfig {
    /// 连接超时
    #[serde(default = "default_connect_timeout", with = "humantime_serde")]
    pub connect_timeout: Duration,
    /// 全局请求超时（含连接+读取），默认 30s（防慢上游拖垮实例）。
    /// 显式设为 null 可关闭（不推荐）；SSE 等流式路径使用独立的无总超时客户端。
    #[serde(default = "default_http_timeout", with = "humantime_serde::option")]
    pub timeout: Option<Duration>,
    /// TCP keepalive 探测间隔（长连接探活，0 = 禁用）
    #[serde(default = "default_tcp_keepalive", with = "humantime_serde")]
    pub tcp_keepalive: Duration,
    /// 每个 host 最大空闲连接数
    #[serde(default = "default_pool_max_idle")]
    pub pool_max_idle_per_host: usize,
    /// 连接池空闲超时
    #[serde(default = "default_pool_idle_timeout", with = "humantime_serde")]
    pub pool_idle_timeout: Duration,
    /// 客户端 TLS 证书路径（mTLS，可选）
    #[serde(default)]
    pub client_cert_path: Option<String>,
    /// 客户端 TLS 私钥路径（mTLS，可选）
    #[serde(default)]
    pub client_key_path: Option<String>,
    /// 上游 CA 证书路径（可选）
    #[serde(default)]
    pub ca_cert_path: Option<String>,
    /// 代理重试次数（0 = 不重试，仅对幂等请求 GET/HEAD）
    #[serde(default)]
    pub retry_max_attempts: u32,
    /// 重试初始退避时间
    #[serde(default = "default_retry_backoff", with = "humantime_serde")]
    pub retry_backoff: Duration,
    /// 每个上游的最大并发请求数（0 = 不限制）。
    /// 用于隔离慢上游，避免单一上游耗尽全局连接/任务（仅 http 代理模式；SSE 为长连接不占名额）。
    #[serde(default)]
    pub max_concurrent_per_upstream: usize,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: default_connect_timeout(),
            timeout: default_http_timeout(),
            tcp_keepalive: default_tcp_keepalive(),
            pool_max_idle_per_host: default_pool_max_idle(),
            pool_idle_timeout: default_pool_idle_timeout(),
            client_cert_path: None,
            client_key_path: None,
            ca_cert_path: None,
            retry_max_attempts: 0,
            retry_backoff: default_retry_backoff(),
            max_concurrent_per_upstream: 0,
        }
    }
}

fn default_retry_backoff() -> Duration {
    Duration::from_millis(100)
}
fn default_http_timeout() -> Option<Duration> {
    Some(Duration::from_secs(30))
}
fn default_tcp_keepalive() -> Duration {
    Duration::from_secs(60)
}
fn default_connect_timeout() -> Duration {
    Duration::from_secs(5)
}
fn default_pool_max_idle() -> usize {
    32
}
fn default_pool_idle_timeout() -> Duration {
    Duration::from_secs(90)
}

// ── 限流配置 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// 每秒允许请求数
    #[serde(default = "default_rate_per_second")]
    pub per_second: u64,
    /// 突发容量
    #[serde(default = "default_rate_burst")]
    pub burst_size: u32,
    /// 全局限流跳过的路径前缀（与 `security_headers.csp_overrides` 同风格，按前缀收窄）。
    /// 命中前缀的请求不消耗全局限流令牌（如 SPA 静态资源 `/assets/*`、`/index.html`），
    /// 其余路径保持 tower-governor 全局限流不变。
    #[serde(default)]
    pub skip_path_prefixes: Vec<String>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            per_second: default_rate_per_second(),
            burst_size: default_rate_burst(),
            skip_path_prefixes: Vec::new(),
        }
    }
}

fn default_rate_per_second() -> u64 {
    50
}
fn default_rate_burst() -> u32 {
    500
}

// ── 认证端点 per-IP 限流配置 ──

/// 认证端点 per-IP 限流（网络层纵深防御，与 IAM 账号锁定互补）。
///
/// - 仅对 `paths` 前缀命中的请求按「来源 IP + 路径前缀」独立计数（令牌桶）；
/// - 默认关闭（`enabled: false`），不影响现有全局限流行为；
/// - 超出 per-IP 桶容量 → 429 + Retry-After，不进入上游；
/// - `trusted_proxies`：LB 后解析 X-Forwarded-For 时跳过的右侧可信代理数（0 = 不信任 XFF）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthRateLimitConfig {
    /// 是否启用
    #[serde(default)]
    pub enabled: bool,
    /// 可信代理数（解析 X-Forwarded-For 时跳过最右侧 N 个条目；0 = 不信任 XFF，仅用对端 IP）
    #[serde(default)]
    pub trusted_proxies: usize,
    /// per-IP 限流档位
    #[serde(default)]
    pub per_ip: IpRateLimitBucket,
    /// 命中即计入的路径前缀
    #[serde(default)]
    pub paths: Vec<String>,
    /// 超限时是否记录审计日志（含 IP、路径、计数）
    #[serde(default = "default_auth_rate_log")]
    pub log_over_limit: bool,
}

impl Default for AuthRateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            trusted_proxies: 0,
            per_ip: IpRateLimitBucket::default(),
            paths: Vec::new(),
            log_over_limit: default_auth_rate_log(),
        }
    }
}

fn default_auth_rate_log() -> bool {
    true
}

/// per-IP 令牌桶档位。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpRateLimitBucket {
    #[serde(default = "default_auth_rate_per_second")]
    pub per_second: u64,
    #[serde(default = "default_auth_rate_burst")]
    pub burst_size: u32,
}

impl Default for IpRateLimitBucket {
    fn default() -> Self {
        Self {
            per_second: default_auth_rate_per_second(),
            burst_size: default_auth_rate_burst(),
        }
    }
}

fn default_auth_rate_per_second() -> u64 {
    5
}
fn default_auth_rate_burst() -> u32 {
    20
}

// ── CORS 配置 ──

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CorsConfig {
    /// 允许的来源列表（空 = 使用 permissive）
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// 是否允许所有来源（仅开发环境）
    #[serde(default)]
    pub permissive: bool,
}

// ── 安全响应头配置 ──

/// 按路径前缀细分的 CSP 覆盖（最长前缀命中优先，未命中回退全局 `content_security_policy`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CspOverrideConfig {
    /// 路径前缀（如 "/admin/templates/design"），请求路径以其开头即命中
    pub path_prefix: String,
    /// 该路径前缀下使用的 Content-Security-Policy
    pub content_security_policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityHeadersConfig {
    /// Content-Security-Policy（全局默认，未命中 csp_overrides 时使用）
    #[serde(default = "default_csp")]
    pub content_security_policy: String,
    /// 按路径前缀细分的 CSP 覆盖（最长前缀优先；空 = 全部使用全局 CSP）
    #[serde(default)]
    pub csp_overrides: Vec<CspOverrideConfig>,
    /// X-Frame-Options
    #[serde(default = "default_frame_options")]
    pub x_frame_options: String,
    /// X-Content-Type-Options
    #[serde(default = "default_content_type_options")]
    pub x_content_type_options: String,
    /// Strict-Transport-Security (max-age 秒数，0 = 不发送)
    #[serde(default = "default_hsts_max_age")]
    pub hsts_max_age: u32,
    /// Referrer-Policy
    #[serde(default = "default_referrer_policy")]
    pub referrer_policy: String,
}

impl Default for SecurityHeadersConfig {
    fn default() -> Self {
        Self {
            content_security_policy: default_csp(),
            csp_overrides: vec![],
            x_frame_options: default_frame_options(),
            x_content_type_options: default_content_type_options(),
            hsts_max_age: default_hsts_max_age(),
            referrer_policy: default_referrer_policy(),
        }
    }
}

fn default_csp() -> String {
    "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'".into()
}
fn default_frame_options() -> String {
    "DENY".into()
}
fn default_content_type_options() -> String {
    "nosniff".into()
}
fn default_hsts_max_age() -> u32 {
    0 // 默认不发送 HSTS（BFF 在 LB 后面，TLS 由 LB 处理）
}
fn default_referrer_policy() -> String {
    "strict-origin-when-cross-origin".into()
}

// ── 多站点（multi-site）配置 ──

const DEFAULT_BIND: &str = "0.0.0.0";
const DEFAULT_SESSION_PROFILE: &str = "default";

fn default_bind() -> String {
    DEFAULT_BIND.to_string()
}

fn is_default_bind(v: &str) -> bool {
    v == DEFAULT_BIND
}

fn default_session_profile() -> String {
    DEFAULT_SESSION_PROFILE.to_string()
}

fn is_default_session_profile(v: &str) -> bool {
    v == DEFAULT_SESSION_PROFILE
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// 显式站点定义（§5.1）。`sites` 为空时按 §5.5 合成 legacy `default` 站点。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteConfig {
    /// 站点名（唯一，`[a-z0-9-]+`，保留名 `admin` 禁止）
    pub name: String,
    /// 监听端口（必填、唯一、≠ `admin_port`、禁用 0）
    pub port: u16,
    /// 监听地址（可选，默认 `0.0.0.0`）
    #[serde(default = "default_bind", skip_serializing_if = "is_default_bind")]
    pub bind: String,
    /// Host 白名单（可选；有效白名单 = `server_names ∪ {public_base_url 主机}`）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub server_names: Vec<String>,
    /// 对外基础 URL（prod 必填；`redirect_uri` 一律由此推导）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_base_url: Option<String>,
    /// 引用的会话 profile（默认 `default`，即顶层 `session`）
    #[serde(
        default = "default_session_profile",
        skip_serializing_if = "is_default_session_profile"
    )]
    pub session_profile: String,
    /// 站点级 SPA 目录（缺省继承顶层 `spa.dir`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spa: Option<SpaConfig>,
    /// 站点 OIDC 绑定
    #[serde(default, skip_serializing_if = "SiteOidcConfig::is_default")]
    pub oidc: SiteOidcConfig,
    /// 登出作用域（默认 `global`）
    #[serde(default, skip_serializing_if = "LogoutScope::is_global")]
    pub logout_scope: LogoutScope,
    /// 站点级安全响应头覆盖（未指定字段继承全局，CSP 一旦指定即整体替换）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_headers: Option<SiteSecurityHeadersOverride>,
}

/// 站点 OIDC 绑定（§5.1）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SiteOidcConfig {
    /// 默认 provider（引用 `oidc.providers[].id`；无 OIDC 站点为空）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_provider: String,
    /// 允许的 provider 集合（缺省 = `[default_provider]`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_providers: Option<Vec<String>>,
}

impl SiteOidcConfig {
    fn is_default(&self) -> bool {
        self.default_provider.is_empty() && self.allowed_providers.is_none()
    }
}

/// 登出作用域：`global` = 登出全部站点；`site` = 仅本站点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogoutScope {
    #[default]
    Global,
    Site,
}

impl LogoutScope {
    fn is_global(&self) -> bool {
        matches!(self, LogoutScope::Global)
    }
}

/// 会话 profile 覆盖（未指定字段继承顶层 `session`，§5.2）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionProfileOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie_name: Option<String>,
    /// `Some("")` 显式强制 host-only；`None` 继承顶层
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie_domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
    #[serde(
        default,
        with = "humantime_serde::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub ttl: Option<Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_unmanaged_subdomains: Option<bool>,
}

/// 站点级安全响应头覆盖（Partial；未指定字段继承全局）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteSecurityHeadersOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_security_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_frame_options: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_content_type_options: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hsts_max_age: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer_policy: Option<String>,
}

/// 继承 + 覆盖后的会话 profile（启动时构建 `SessionManagerLayer` 的依据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSessionProfile {
    pub name: String,
    pub cookie_name: String,
    pub cookie_domain: Option<String>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: String,
    pub ttl: Option<Duration>,
    pub allow_unmanaged_subdomains: bool,
}

/// 归一化 + 合成后的站点（运行时按名解析）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSite {
    pub name: String,
    pub port: u16,
    pub bind: String,
    pub server_names: Vec<String>,
    pub public_base_url: Option<String>,
    pub public_host: Option<String>,
    pub allowed_hosts: Vec<String>,
    pub spa_dir: String,
    pub session_profile: String,
    pub default_provider: String,
    pub allowed_providers: Vec<String>,
    pub logout_scope: LogoutScope,
    pub security_headers: Option<SiteSecurityHeadersOverride>,
    pub legacy: bool,
}

/// 归一化主机名：trim、小写、去尾点、`[::1]` 去方括号。
///
/// 含 `://`、`/`、`*`、`:`（端口/裸 IPv6）、空白/控制字符或为空 → `None`。
pub fn normalize_host(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    if trimmed.contains("://") || trimmed.contains('/') || trimmed.contains('*') {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let stripped = lower.trim_end_matches('.');
    if stripped.is_empty() {
        return None;
    }
    let host = if stripped.starts_with('[') && stripped.ends_with(']') {
        let inner = &stripped[1..stripped.len() - 1];
        if inner.is_empty() {
            return None;
        }
        inner
    } else if stripped.contains(':') {
        return None;
    } else {
        stripped
    };
    Some(host.to_string())
}

/// 归一化 cookie 域：小写、去前导点；空串 → `Some("")`（host-only）；非法字符 → `None`。
pub fn normalize_cookie_domain(raw: &str) -> Option<String> {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return Some(String::new());
    }
    let domain = lower.trim_start_matches('.');
    if domain.is_empty() {
        return None; // 仅含点的非法域
    }
    if domain
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        Some(domain.to_string())
    } else {
        None
    }
}

/// Domain cookie 与主机是否匹配（`domain` 非空且 host 等于它或为其子域）。
pub fn cookie_domain_matches(domain: &str, host: &str) -> bool {
    !domain.is_empty() && (host == domain || host.ends_with(&format!(".{}", domain)))
}

/// 归一化 `public_base_url`，返回 `(去尾斜杠 URL, 归一化主机)`。
///
/// 仅接受 http(s)、必须有主机、path 仅允许空或 `/`、无 query/fragment。
pub fn normalize_public_base_url(raw: &str) -> anyhow::Result<(String, String)> {
    let parsed =
        url::Url::parse(raw).map_err(|e| anyhow::anyhow!("public_base_url 非法: {}", e))?;
    anyhow::ensure!(
        parsed.scheme() == "http" || parsed.scheme() == "https",
        "public_base_url 必须为 http(s) URL: {}",
        raw
    );
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("public_base_url 缺少主机名: {}", raw))?;
    anyhow::ensure!(
        parsed.query().is_none(),
        "public_base_url 不允许 query: {}",
        raw
    );
    anyhow::ensure!(
        parsed.fragment().is_none(),
        "public_base_url 不允许 fragment: {}",
        raw
    );
    let path = parsed.path();
    anyhow::ensure!(
        path.is_empty() || path == "/",
        "public_base_url 不允许 path: {}",
        raw
    );
    let host = normalize_host(host)
        .ok_or_else(|| anyhow::anyhow!("public_base_url 主机名非法: {}", raw))?;
    let mut normalized = parsed;
    normalized.set_path("");
    normalized.set_query(None);
    normalized.set_fragment(None);
    Ok((normalized.as_str().trim_end_matches('/').to_string(), host))
}

/// 归一化可选 `public_base_url`：非法值保留原文（由启动期校验报错），不返回主机。
fn resolve_public_base_url(raw: Option<&str>) -> (Option<String>, Option<String>) {
    match raw {
        Some(raw) => match normalize_public_base_url(raw) {
            Ok((url, host)) => (Some(url), Some(host)),
            Err(_) => (Some(raw.to_string()), None),
        },
        None => (None, None),
    }
}

/// 顶层 `cookie_domain`：缺省/空串 → host-only（`None`）；非空归一化（非法 → `None`）。
fn resolve_cookie_domain(raw: Option<&str>) -> Option<String> {
    match raw {
        None => None,
        Some("") => None,
        Some(s) => normalize_cookie_domain(s).filter(|d| !d.is_empty()),
    }
}

fn push_unique_host(hosts: &mut Vec<String>, host: &str) {
    if !hosts.iter().any(|h| h == host) {
        hosts.push(host.to_string());
    }
}

/// 是否生产环境（`BFF_ENV=prod`）。prod 判定通过 `validate_with_env` 注入，测试无需改环境变量。
pub(crate) fn is_prod_env() -> bool {
    std::env::var("BFF_ENV")
        .map(|v| v == "prod")
        .unwrap_or(false)
}

/// profile 名是否匹配 `[a-z0-9-]+`。
fn is_valid_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// 站点名是否匹配 `[a-z0-9-]+`（与 profile 名同规则）。
fn is_valid_site_name(name: &str) -> bool {
    is_valid_profile_name(name)
}

/// 归一化路由路径用于重复规格检测：去尾斜杠（与 `match_route` 的段边界一致）。
fn normalize_route_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 校验错误中的 profile 字段路径（`default` 归属顶层 `session`）。
fn profile_field_path(name: &str, field: &str) -> String {
    if name == DEFAULT_SESSION_PROFILE {
        format!("session.{}", field)
    } else {
        format!("session_profiles[{}].{}", name, field)
    }
}

impl AppConfig {
    /// 合成站点列表（§5.5）：无 `sites` 时合成 legacy `default` 站点。
    ///
    /// 归一化与并集在此集中完成（`server_names` 过滤非法项并归一化；
    /// `allowed_hosts = server_names ∪ {public_host}` 去重保序）；非法值的报错由启动期校验负责。
    pub fn effective_sites(&self) -> Vec<ResolvedSite> {
        if self.sites.is_empty() {
            let default_provider = self
                .oidc
                .providers
                .first()
                .map(|p| p.id.clone())
                .unwrap_or_default();
            let allowed_providers = self.oidc.providers.iter().map(|p| p.id.clone()).collect();
            let server_names: Vec<String> = self
                .server
                .trusted_hosts
                .iter()
                .filter_map(|h| normalize_host(h))
                .collect();
            let (public_base_url, public_host) =
                resolve_public_base_url(self.server.public_base_url.as_deref());
            let mut allowed_hosts = server_names.clone();
            if let Some(host) = &public_host {
                push_unique_host(&mut allowed_hosts, host);
            }
            vec![ResolvedSite {
                name: "default".into(),
                port: self.server.business_port,
                bind: default_bind(),
                server_names,
                public_base_url,
                public_host,
                allowed_hosts,
                spa_dir: self.spa.dir.clone(),
                session_profile: default_session_profile(),
                default_provider,
                allowed_providers,
                logout_scope: LogoutScope::Global,
                security_headers: None,
                legacy: true,
            }]
        } else {
            self.sites
                .iter()
                .map(|site| {
                    let server_names: Vec<String> = site
                        .server_names
                        .iter()
                        .filter_map(|h| normalize_host(h))
                        .collect();
                    let (public_base_url, public_host) =
                        resolve_public_base_url(site.public_base_url.as_deref());
                    let mut allowed_hosts = server_names.clone();
                    if let Some(host) = &public_host {
                        push_unique_host(&mut allowed_hosts, host);
                    }
                    let allowed_providers =
                        site.oidc.allowed_providers.clone().unwrap_or_else(|| {
                            if site.oidc.default_provider.is_empty() {
                                Vec::new()
                            } else {
                                vec![site.oidc.default_provider.clone()]
                            }
                        });
                    ResolvedSite {
                        name: site.name.clone(),
                        port: site.port,
                        bind: site.bind.clone(),
                        server_names,
                        public_base_url,
                        public_host,
                        allowed_hosts,
                        spa_dir: site
                            .spa
                            .as_ref()
                            .map(|s| s.dir.clone())
                            .unwrap_or_else(|| self.spa.dir.clone()),
                        session_profile: site.session_profile.clone(),
                        default_provider: site.oidc.default_provider.clone(),
                        allowed_providers,
                        logout_scope: site.logout_scope,
                        security_headers: site.security_headers.clone(),
                        legacy: false,
                    }
                })
                .collect()
        }
    }

    /// 解析全部会话 profile（§5.2）：`default` = 顶层 `session`，其余逐字段继承 + 覆盖。
    pub fn resolved_session_profiles(&self) -> HashMap<String, ResolvedSessionProfile> {
        let default = ResolvedSessionProfile {
            name: "default".into(),
            cookie_name: self.session.cookie_name.clone(),
            cookie_domain: resolve_cookie_domain(self.session.cookie_domain.as_deref()),
            secure: self.session.secure,
            http_only: self.session.http_only,
            same_site: self.session.same_site.clone(),
            ttl: self.session.ttl,
            allow_unmanaged_subdomains: self.session.allow_unmanaged_subdomains,
        };
        let mut profiles = HashMap::new();
        profiles.insert("default".to_string(), default.clone());
        for (name, over) in &self.session_profiles {
            // `default` 只能由顶层 `session` 定义（重复定义由启动期校验拒绝）
            if name == "default" {
                continue;
            }
            profiles.insert(
                name.clone(),
                ResolvedSessionProfile {
                    name: name.clone(),
                    cookie_name: over
                        .cookie_name
                        .clone()
                        .unwrap_or_else(|| default.cookie_name.clone()),
                    cookie_domain: match &over.cookie_domain {
                        None => default.cookie_domain.clone(),
                        Some(raw) => resolve_cookie_domain(Some(raw.as_str())),
                    },
                    secure: over.secure.unwrap_or(default.secure),
                    http_only: over.http_only.unwrap_or(default.http_only),
                    same_site: over
                        .same_site
                        .clone()
                        .unwrap_or_else(|| default.same_site.clone()),
                    ttl: over.ttl.or(default.ttl),
                    allow_unmanaged_subdomains: over
                        .allow_unmanaged_subdomains
                        .unwrap_or(default.allow_unmanaged_subdomains),
                },
            );
        }
        profiles
    }
}

// ── 请求体大小限制 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BodyLimitConfig {
    /// 请求体最大字节数（统一作用于代理 / pipeline / script / 管理面）
    #[serde(default = "default_body_limit")]
    pub max_bytes: usize,
    /// 代理响应体最大字节数（防大响应内存膨胀/OOM）
    #[serde(default = "default_response_limit")]
    pub max_response_bytes: usize,
}

impl Default for BodyLimitConfig {
    fn default() -> Self {
        Self {
            max_bytes: default_body_limit(),
            max_response_bytes: default_response_limit(),
        }
    }
}

fn default_body_limit() -> usize {
    10 * 1024 * 1024 // 10 MiB
}

fn default_response_limit() -> usize {
    64 * 1024 * 1024 // 64 MiB
}

// ── 熔断器配置 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    /// 失败阈值（滚动窗口内失败次数，非“连续失败”——间歇性故障同样会累积触发）
    #[serde(default = "default_cb_failure_threshold")]
    pub failure_threshold: u32,
    /// 失败计数滚动窗口（窗口外的失败自动衰减）
    #[serde(default = "default_cb_failure_window", with = "humantime_serde")]
    pub failure_window: Duration,
    /// 熔断打开持续时间
    #[serde(default = "default_cb_open_duration", with = "humantime_serde")]
    pub open_duration: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: default_cb_failure_threshold(),
            failure_window: default_cb_failure_window(),
            open_duration: default_cb_open_duration(),
        }
    }
}

fn default_cb_failure_threshold() -> u32 {
    5
}
fn default_cb_failure_window() -> Duration {
    Duration::from_secs(60)
}
fn default_cb_open_duration() -> Duration {
    Duration::from_secs(30)
}

// ── 脚本引擎配置 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptingConfig {
    /// 脚本最大执行时长
    #[serde(default = "default_script_max_duration", with = "humantime_serde")]
    pub max_duration: Duration,
}

impl Default for ScriptingConfig {
    fn default() -> Self {
        Self {
            max_duration: default_script_max_duration(),
        }
    }
}

fn default_script_max_duration() -> Duration {
    Duration::from_secs(2)
}

// ── WebSocket 隧道配置 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketTunnelConfig {
    /// 上游握手连接超时
    #[serde(default = "default_ws_connect_timeout", with = "humantime_serde")]
    pub connect_timeout: Duration,
    /// 空闲超时：双向均无消息超过该时长则关闭（0 = 禁用）
    #[serde(default = "default_ws_idle_timeout", with = "humantime_serde")]
    pub idle_timeout: Duration,
    /// 心跳间隔（周期向对端发 Ping；0 = 禁用）
    #[serde(default = "default_ws_heartbeat", with = "humantime_serde")]
    pub heartbeat_interval: Duration,
    /// 单条消息最大字节数
    #[serde(default = "default_ws_max_message")]
    pub max_message_bytes: usize,
}

impl Default for WebSocketTunnelConfig {
    fn default() -> Self {
        Self {
            connect_timeout: default_ws_connect_timeout(),
            idle_timeout: default_ws_idle_timeout(),
            heartbeat_interval: default_ws_heartbeat(),
            max_message_bytes: default_ws_max_message(),
        }
    }
}

fn default_ws_connect_timeout() -> Duration {
    Duration::from_secs(5)
}
fn default_ws_idle_timeout() -> Duration {
    Duration::from_secs(300)
}
fn default_ws_heartbeat() -> Duration {
    Duration::from_secs(30)
}
fn default_ws_max_message() -> usize {
    1024 * 1024 // 1 MiB
}

// ── OTel（OTLP）遥测配置 ──

/// OpenTelemetry 追踪导出配置。
///
/// `otlp_endpoint` 为空（默认）时**完全禁用导出**：不注册导出层、无网络出站，
/// 仅保留 W3C `traceparent` 注入/传播（未启用导出时行为不变）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    /// OTLP/gRPC 出口（如 `http://otel-collector:4317`；`https` 走 rustls）。留空禁用。
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
    /// 资源属性 `service.name`
    #[serde(default = "default_telemetry_service_name")]
    pub service_name: String,
    /// 根采样率 0.0–1.0（ParentBased：有上游上下文时跟随上游采样位，W3C 语义）
    #[serde(default = "default_telemetry_sample_ratio")]
    pub sample_ratio: f64,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            otlp_endpoint: None,
            service_name: default_telemetry_service_name(),
            sample_ratio: default_telemetry_sample_ratio(),
        }
    }
}

fn default_telemetry_service_name() -> String {
    "bff".into()
}

fn default_telemetry_sample_ratio() -> f64 {
    1.0
}

// ── 配置持久化 ──

/// 管理端热更新落盘与外部变更热重载。
///
/// 开启后管理端写操作（导入配置/路由/provider/pipeline/脚本）会先把
/// **脱敏后的完整配置**原子写入 `path`，再应用内存；重启/多副本可据此收敛。
/// 文件中的 `***` 哨兵在加载时按环境变量/基础配置回填真实密钥。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistenceConfig {
    /// 是否启用（生产建议 true；默认 false 避免开发/测试意外落盘）
    #[serde(default)]
    pub enabled: bool,
    /// 持久化文件路径
    #[serde(default = "default_persistence_path")]
    pub path: String,
    /// 外部变更轮询间隔（多副本共享存储时用于收敛；0 = 关闭轮询）
    #[serde(default = "default_persistence_watch", with = "humantime_serde")]
    pub watch_interval: Duration,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: default_persistence_path(),
            watch_interval: default_persistence_watch(),
        }
    }
}

fn default_persistence_path() -> String {
    "config/state/runtime.yaml".into()
}
fn default_persistence_watch() -> Duration {
    Duration::from_secs(5)
}

// ── 健康检查配置 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthConfig {
    /// 就绪检查中需探测的上游列表（显式声明）
    /// 如果为空，则自动从 routes 中提取所有 proxy 类路由的 upstream 去重
    #[serde(default)]
    pub upstreams: Vec<String>,
    /// 探测结果缓存时长（避免探针风暴与上游抖动放大；0 = 不缓存）
    #[serde(default = "default_probe_cache_ttl", with = "humantime_serde")]
    pub cache_ttl: Duration,
    /// 每次探测的超时时间
    #[serde(default = "default_probe_timeout", with = "humantime_serde")]
    pub probe_timeout: Duration,
    /// 允许部分上游不可达时仍返回 ready（true = degraded 模式仍 200）
    #[serde(default)]
    pub allow_degraded: bool,
    /// 探测路径（默认 "/"）
    #[serde(default = "default_probe_path")]
    pub probe_path: String,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            upstreams: vec![],
            cache_ttl: default_probe_cache_ttl(),
            probe_timeout: default_probe_timeout(),
            allow_degraded: false,
            probe_path: default_probe_path(),
        }
    }
}

fn default_probe_cache_ttl() -> Duration {
    Duration::from_secs(1)
}
fn default_probe_timeout() -> Duration {
    Duration::from_secs(2)
}
fn default_probe_path() -> String {
    "/".into()
}

// ── Provider 配置 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default = "default_provider_kind")]
    pub session_store: String,
    #[serde(default = "default_provider_kind")]
    pub cache: String,
    #[serde(default = "default_provider_kind")]
    pub lock: String,
    #[serde(default)]
    pub redis_url: String,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            session_store: default_provider_kind(),
            cache: default_provider_kind(),
            lock: default_provider_kind(),
            redis_url: String::new(),
        }
    }
}

fn default_provider_kind() -> String {
    "memory".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default = "default_cookie_name")]
    pub cookie_name: String,
    /// Cookie 作用域：非空 = Domain cookie（跨子域共享）；缺省/空串 = host-only
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie_domain: Option<String>,
    #[serde(default)]
    pub secure: bool,
    #[serde(default = "default_true")]
    pub http_only: bool,
    #[serde(default = "default_same_site")]
    pub same_site: String,
    /// 会话空闲过期时间。与 Cookie `Max-Age` 和服务端存储 TTL 对齐；
    /// 设为 null 则退回浏览器会话级 Cookie + 服务端默认 2 周（不推荐）。
    #[serde(default = "default_session_ttl", with = "humantime_serde::option")]
    pub ttl: Option<Duration>,
    /// `sessions` 索引（管理端列表）GC 周期：按会话存储实际存在性清理，默认 10 分钟。
    #[serde(default = "default_session_gc_interval", with = "humantime_serde")]
    pub gc_interval: Duration,
    /// 共享域信任边界显式确认（prod 下 cookie_domain 非空时必须为 true）
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_unmanaged_subdomains: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            cookie_name: default_cookie_name(),
            cookie_domain: None,
            secure: true, // 默认安全
            http_only: true,
            same_site: default_same_site(),
            ttl: default_session_ttl(),
            gc_interval: default_session_gc_interval(),
            allow_unmanaged_subdomains: false,
        }
    }
}

fn default_session_ttl() -> Option<Duration> {
    Some(Duration::from_secs(14 * 24 * 3600))
}
fn default_session_gc_interval() -> Duration {
    Duration::from_secs(600)
}

fn default_cookie_name() -> String {
    "BFF_SESSION".into()
}
fn default_true() -> bool {
    true
}
fn default_same_site() -> String {
    // 跨站点 IdP 回调是跨站顶层导航：Strict 会丢会话 Cookie 导致登录失败
    // （Keycloak 真实 IdP 契约验证实测）；Lax 为 OIDC RP 的通行默认，
    // CSRF 由授权流程的 state + PKCE 把关。同站 IdP 部署可显式改 Strict。
    "Lax".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminConfig {
    #[serde(default = "default_ip_whitelist")]
    pub ip_whitelist: Vec<String>,
    #[serde(default = "default_auth_mode")]
    pub auth_mode: String, // token | none
    #[serde(default = "default_auth_token")]
    pub auth_token: String,
    /// 是否启用 test/eval 端点（生产环境建议 false）
    #[serde(default = "default_true")]
    pub enable_test_endpoints: bool,
    /// test/eval 端点每分钟每 IP 最大请求数
    #[serde(default = "default_test_rate_limit")]
    pub test_endpoint_rate_limit: u32,
    /// 管理 API 请求体上限（与业务 body_limit 独立）
    #[serde(default = "default_admin_body_limit")]
    pub max_body_bytes: usize,
    /// 管理 token 认证失败限流：每个来源 IP 每分钟允许的失败次数，超出 → 429
    #[serde(default = "default_admin_auth_fail_limit")]
    pub auth_fail_limit_per_minute: u32,
    /// 管理白名单 / 失败限流解析客户端 IP 时信任的代理跳数（0 = 不信任 XFF）
    #[serde(default)]
    pub trusted_proxies: usize,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            ip_whitelist: default_ip_whitelist(),
            auth_mode: default_auth_mode(),
            auth_token: default_auth_token(),
            enable_test_endpoints: true,
            test_endpoint_rate_limit: default_test_rate_limit(),
            max_body_bytes: default_admin_body_limit(),
            auth_fail_limit_per_minute: default_admin_auth_fail_limit(),
            trusted_proxies: 0,
        }
    }
}

fn default_admin_body_limit() -> usize {
    8 * 1024 * 1024 // 8 MiB
}
fn default_admin_auth_fail_limit() -> u32 {
    30
}

fn default_ip_whitelist() -> Vec<String> {
    vec!["127.0.0.1".into(), "::1".into()]
}
fn default_auth_mode() -> String {
    "token".into()
}
fn default_auth_token() -> String {
    "changeme".into()
}
fn default_test_rate_limit() -> u32 {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaConfig {
    #[serde(default = "default_spa_dir")]
    pub dir: String,
}

impl Default for SpaConfig {
    fn default() -> Self {
        Self {
            dir: default_spa_dir(),
        }
    }
}

fn default_spa_dir() -> String {
    "frontend/dist".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OidcSection {
    #[serde(default)]
    pub providers: Vec<OidcProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcProviderConfig {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    pub issuer_url: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    /// 回调路径，默认 `/auth/callback`
    #[serde(default = "default_callback_path")]
    pub callback_path: String,
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
    /// 仅开发/测试：跳过 ID Token 签名验证（仍校验 nonce 与过期时间）
    #[serde(default)]
    pub insecure_skip_id_token_verification: bool,
    /// 令牌提前刷新的余量秒数
    #[serde(default = "default_refresh_skew")]
    pub refresh_skew_secs: u64,
    /// 允许该 provider 被多个站点共享（默认 false = 站点间令牌隔离的结构性约束）
    #[serde(default, skip_serializing_if = "is_false")]
    pub shared_across_sites: bool,
}

fn default_callback_path() -> String {
    "/auth/callback".into()
}
fn default_scopes() -> Vec<String> {
    vec!["openid".into()]
}
fn default_refresh_skew() -> u64 {
    60
}

/// 令牌刷新中间件配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRefreshConfig {
    /// 无需刷新检查的路径前缀列表
    #[serde(default = "default_token_refresh_skip_prefixes")]
    pub skip_prefixes: Vec<String>,
}

impl Default for TokenRefreshConfig {
    fn default() -> Self {
        Self {
            skip_prefixes: default_token_refresh_skip_prefixes(),
        }
    }
}

fn default_token_refresh_skip_prefixes() -> Vec<String> {
    vec![
        "/login".into(),
        "/auth/callback".into(),
        "/logout".into(),
        "/live".into(),
        "/ready".into(),
        "/assets".into(),
        "/ws".into(),
        "/sse".into(),
    ]
}

/// 编排定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineDef {
    #[serde(default)]
    pub strategy: StrategyDef,
    pub steps: Vec<StepDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyDef {
    /// 整体超时
    #[serde(default = "default_strategy_timeout", with = "humantime_serde")]
    pub timeout: Duration,
    #[serde(default = "default_error_handling")]
    pub error_handling: String, // fail_fast | continue
}

impl Default for StrategyDef {
    fn default() -> Self {
        Self {
            timeout: default_strategy_timeout(),
            error_handling: default_error_handling(),
        }
    }
}

fn default_strategy_timeout() -> Duration {
    Duration::from_secs(10)
}
fn default_error_handling() -> String {
    "fail_fast".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepDef {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: StepType,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub config: StepConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepType {
    HttpRequest,
    Script,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StepConfig {
    // http_request
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_step_method")]
    pub method: String,
    #[serde(default, with = "humantime_serde::option")]
    pub timeout: Option<Duration>,
    #[serde(default, with = "humantime_serde::option")]
    pub cache_ttl: Option<Duration>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub body: Option<String>,
    // script
    #[serde(default)]
    pub script: Option<String>,
}

fn default_step_method() -> String {
    "GET".into()
}

fn default_proxy_mode() -> String {
    "http".into()
}

/// 配置导出时用于替换敏感字段的哨兵值（导入时识别并跳过覆盖）。
pub const SECRET_SENTINEL: &str = "***";

fn default_subject_token_type() -> String {
    "urn:ietf:params:oauth:token-type:access_token".into()
}

fn default_exchange_cache_ttl() -> Duration {
    Duration::from_secs(30)
}

/// RFC 8693 Token Exchange 客户端认证方式（二选一）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenExchangeAuthMethod {
    /// `Authorization: Basic base64(client_id:client_secret)`（默认，推荐）
    #[default]
    ClientSecretBasic,
    /// form 中携带 `client_id` + `client_secret`
    ClientSecretPost,
}

/// 代理路由的 RFC 8693 Token Exchange 配置段（可选）。
///
/// 启用后：以会话 access token 为 `subject_token` 向 `token_endpoint` 交换
/// 面向上游资源的 access token，并作为 `Authorization: Bearer` 注入代理请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenExchangeConfig {
    /// 授权服务器 token endpoint；缺省回退会话 provider discovery 的 token endpoint（§4.4）
    #[serde(default)]
    pub token_endpoint: Option<String>,
    /// 交换客户端标识（必填）
    #[serde(default)]
    pub client_id: String,
    /// 交换客户端密钥，支持 `${ENV:default}` 环境变量注入（导出时打码）
    #[serde(default, deserialize_with = "deserialize_env_or_default")]
    pub client_secret: String,
    /// 客户端认证方式（默认 `client_secret_basic`）
    #[serde(default)]
    pub client_auth_method: TokenExchangeAuthMethod,
    /// subject token 类型 URN（默认 access_token）
    #[serde(default = "default_subject_token_type")]
    pub subject_token_type: String,
    /// 请求的 audience（RFC 8693，可重复）
    #[serde(default)]
    pub audience: Vec<String>,
    /// 交换后收窄 scope（空格分隔，可选）
    #[serde(default)]
    pub scope: String,
    /// 期望返回的 token 类型 URN（默认 access_token）
    #[serde(default = "default_subject_token_type")]
    pub requested_token_type: String,
    /// 委托场景预留（本期仅占位，无 actor_token 值来源）
    #[serde(default)]
    pub actor_token_type: String,
    /// 交换结果缓存时长（实际受 `expires_in` 与后端 TTL 上限约束，§6.2）
    #[serde(default = "default_exchange_cache_ttl", with = "humantime_serde")]
    pub cache_ttl: Duration,
}

impl Default for TokenExchangeConfig {
    fn default() -> Self {
        Self {
            token_endpoint: None,
            client_id: String::new(),
            client_secret: String::new(),
            client_auth_method: TokenExchangeAuthMethod::default(),
            subject_token_type: default_subject_token_type(),
            audience: Vec::new(),
            scope: String::new(),
            requested_token_type: default_subject_token_type(),
            actor_token_type: String::new(),
            cache_ttl: default_exchange_cache_ttl(),
        }
    }
}

impl TokenExchangeConfig {
    /// 缓存键的配置指纹（§6.1）：`token_endpoint | client_id | audience | scope | requested_token_type`。
    pub fn fingerprint(&self) -> String {
        let mut s = String::new();
        s.push_str(self.token_endpoint.as_deref().unwrap_or(""));
        s.push('|');
        s.push_str(&self.client_id);
        s.push('|');
        s.push_str(&self.audience.join(","));
        s.push('|');
        s.push_str(&self.scope);
        s.push('|');
        s.push_str(&self.requested_token_type);
        s
    }
}

/// 路由定义（统一模型）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteDef {
    /// 路由匹配路径前缀，如 "/api/users"、"/api/dashboard"
    pub path: String,

    /// 站点归属（缺省/空 = 全部站点，兼容旧配置）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<String>,

    /// HTTP 方法过滤（空 = 全部）
    #[serde(default)]
    pub methods: Vec<String>,

    /// 路由描述（Admin-UI 展示用）
    #[serde(default)]
    pub description: String,

    /// 是否需要 OIDC 认证
    #[serde(default = "default_true")]
    pub auth_required: bool,

    /// 路由类型
    #[serde(rename = "type")]
    pub route_type: RouteType,

    /// 类型专属配置
    #[serde(default)]
    pub config: RouteTypeConfig,

    /// 输入映射：调用方传参 → 执行引擎输入
    #[serde(default)]
    pub input_mapping: InputMapping,

    /// 输出映射：执行引擎输出 → 响应格式（可选，默认透传）
    #[serde(default)]
    pub output_mapping: OutputMapping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteType {
    /// 反向代理
    Proxy,
    /// DAG 编排（引用 pipeline 定义）
    Pipeline,
    /// 脚本直调
    Script,
    /// 静态响应 / Mock
    Static,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RouteTypeConfig {
    // ---- Proxy 专属 ----
    pub upstream: Option<String>,
    #[serde(default)]
    pub strip_prefix: bool,
    /// 熔断阈值（滚动窗口内失败次数；0 = 该路由不熔断，使用全局默认）
    #[serde(default)]
    pub circuit_breaker_threshold: u32,
    /// 代理模式: "http" | "sse" | "websocket" | "auto"
    /// - http: 一次性请求-响应（默认）
    /// - sse: 流式透传 SSE
    /// - websocket: WebSocket 双向隧道
    /// - auto: 自动检测（根据 Upgrade/Content-Type）
    #[serde(default = "default_proxy_mode")]
    pub proxy_mode: String,

    /// 路由级请求超时（覆盖 http_client.timeout；仅 proxy http 模式生效）。
    /// 如上传/导出类慢接口可单独放宽。
    #[serde(default, with = "humantime_serde::option")]
    pub timeout: Option<Duration>,

    /// 是否向浏览器透传上游 `set-cookie`（默认 false，防上游/被攻破服务植入 Cookie）。
    #[serde(default)]
    pub forward_set_cookie: bool,

    /// RFC 8693 Token Exchange（代理上游认证的前置交换，可选）。
    /// 启用后以会话 access token 交换面向上游资源的 token 再注入代理请求。
    #[serde(default)]
    pub token_exchange: Option<TokenExchangeConfig>,

    // ---- Pipeline 专属 ----
    /// 引用 pipeline 名称（指向 pipelines 注册表）
    pub pipeline: Option<String>,
    /// 内联 pipeline 定义（与 pipeline 二选一）
    pub pipeline_inline: Option<PipelineDef>,

    // ---- Script 专属 ----
    /// 引用脚本名称（指向 scripts 注册表）
    pub script: Option<String>,
    /// 内联脚本（与 script 二选一）
    pub script_inline: Option<String>,

    // ---- Static 专属 ----
    pub status: Option<u16>,
    pub body: Option<serde_json::Value>,
    pub headers: Option<HashMap<String, String>>,
}

/// 输入映射规则
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InputMapping {
    /// 从 query string 提取，如 { "userId": "query.id" }
    #[serde(default)]
    pub from_query: HashMap<String, String>,

    /// 从请求体 JSON 路径提取，如 { "name": "body.user.name" }
    #[serde(default)]
    pub from_body: HashMap<String, String>,

    /// 从路径提取，如 { "userId": "path./api/users/{userId}" }
    #[serde(default)]
    pub from_path: HashMap<String, String>,

    /// 从 Header 提取
    #[serde(default)]
    pub from_header: HashMap<String, String>,

    /// 从 OIDC Session 提取。
    ///
    /// 上下文为**扁平**对象：`{ "sub": ..., "provider": ..., "access_token": ... }`，
    /// 因此路径写 `sub` / `provider`（文档曾误写为 `session.sub`，会解析为 Null 静默丢弃）。
    #[serde(default)]
    pub from_session: HashMap<String, String>,

    /// 从环境变量提取，如 { "region": "env.AWS_REGION" }
    #[serde(default)]
    pub from_env: HashMap<String, String>,

    /// 常量默认值
    #[serde(default)]
    pub defaults: HashMap<String, serde_json::Value>,
}

/// 输出映射规则
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputMapping {
    /// 是否包裹在 { "data": ..., "code": 0 } 等统一响应体中
    pub wrap: Option<String>,

    /// 状态码映射（执行结果 → HTTP 状态码）
    #[serde(default)]
    pub status_map: HashMap<String, u16>,

    /// 字段重命名
    #[serde(default)]
    pub rename: HashMap<String, String>,

    /// 字段过滤（白名单）
    #[serde(default)]
    pub pick: Vec<String>,
}

impl AppConfig {
    /// 按 ADR 的层次从配置目录加载。
    pub fn load(config_dir: &Path) -> anyhow::Result<Self> {
        let mut fig = Figment::new()
            .merge(Yaml::file(config_dir.join("base.yaml")))
            .merge(Yaml::file(config_dir.join("oidc/providers.yaml")));

        // pipelines/*.yaml：每个文件顶层为 `name -> PipelineDef`，统一挂到 `pipelines` 键下
        let mut pipelines: HashMap<String, serde_yaml::Value> = HashMap::new();
        let pipelines_dir = config_dir.join("pipelines");
        if pipelines_dir.is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&pipelines_dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .map(|e| e == "yaml" || e == "yml")
                        .unwrap_or(false)
                })
                .collect();
            files.sort();
            for f in files {
                let content = std::fs::read_to_string(&f)?;
                let map: HashMap<String, serde_yaml::Value> = serde_yaml::from_str(&content)
                    .map_err(|e| anyhow::anyhow!("解析 {:?} 失败: {}", f, e))?;
                pipelines.extend(map);
            }
        }
        let mut pipelines_root = HashMap::new();
        pipelines_root.insert("pipelines".to_string(), pipelines);
        fig = fig.merge(Serialized::defaults(pipelines_root));

        fig = fig.merge(Yaml::file(config_dir.join("routes/routes.yaml")));

        // env 文件优先级低于 `BFF_*` 环境变量（环境变量最高，符合 12-factor）
        if let Ok(env) = std::env::var("BFF_ENV") {
            let env_file = config_dir.join("env").join(format!("{}.yaml", env));
            if env_file.is_file() {
                fig = fig.merge(Yaml::file(env_file));
            }
        }

        fig = fig.merge(Env::prefixed("BFF_").split("__"));

        let mut cfg: AppConfig = fig.extract()?;

        // 持久化配置覆盖（管理端落盘的完整配置）
        // 优先级：base/分文件 < runtime.yaml < BFF_* 环境变量。
        // runtime.yaml 为脱敏快照（密钥为 *** 哨兵）→ 按当前基础配置回填后再合并。
        if cfg.persistence.enabled {
            let state_path = PathBuf::from(&cfg.persistence.path);
            if state_path.is_file() {
                let raw = std::fs::read_to_string(&state_path)?;
                let mut overlay: AppConfig = serde_yaml::from_str(&raw)
                    .map_err(|e| anyhow::anyhow!("解析持久化配置 {:?} 失败: {}", state_path, e))?;
                overlay.merge_sensitive_secrets(&cfg);
                let merged = Figment::from(Serialized::defaults(&overlay))
                    .merge(Env::prefixed("BFF_").split("__"));
                cfg = merged.extract()?;
            }
        }

        cfg.validate()?;
        Ok(cfg)
    }

    /// 使用进程环境（`BFF_ENV=prod`）判定是否执行生产环境校验。
    pub fn validate(&self) -> anyhow::Result<()> {
        self.validate_with_env(is_prod_env())
    }

    /// 校验入口：`is_prod` 由调用方注入（测试可并行安全地传 `true`/`false`）。
    pub fn validate_with_env(&self, is_prod: bool) -> anyhow::Result<()> {
        // bff_secret 校验（必须最先，因为后续 crypto::init 依赖它）
        anyhow::ensure!(
            !self.bff_secret.secret.is_empty(),
            "bff_secret.secret 不能为空"
        );
        anyhow::ensure!(
            self.bff_secret.salt.len() >= 16,
            "bff_secret.salt 长度不足（需要 ≥16 字节，当前 {} 字节）",
            self.bff_secret.salt.len()
        );

        // Provider 类型校验（支持 memory | redis）
        for (label, kind) in [
            ("session_store", &self.provider.session_store),
            ("cache", &self.provider.cache),
            ("lock", &self.provider.lock),
        ] {
            anyhow::ensure!(
                kind == "memory" || kind == "redis",
                "provider.{} 仅支持 memory | redis，收到: {}",
                label,
                kind
            );
        }
        let uses_redis = [
            &self.provider.session_store,
            &self.provider.cache,
            &self.provider.lock,
        ]
        .iter()
        .any(|k| k.as_str() == "redis");
        if uses_redis {
            anyhow::ensure!(
                !self.provider.redis_url.is_empty(),
                "provider.* 配置 redis 时必须配置 provider.redis_url"
            );
            anyhow::ensure!(
                redis::Client::open(self.provider.redis_url.as_str()).is_ok(),
                "provider.redis_url 非法: {}",
                self.provider.redis_url
            );
        }

        // 端口范围校验
        anyhow::ensure!(
            self.server.business_port > 0 && self.server.business_port != self.server.admin_port,
            "业务端口与管理端口不能相同"
        );

        // public_base_url 校验
        if let Some(base) = &self.server.public_base_url {
            let parsed = url::Url::parse(base)
                .map_err(|e| anyhow::anyhow!("server.public_base_url 非法: {}", e))?;
            anyhow::ensure!(
                parsed.scheme() == "http" || parsed.scheme() == "https",
                "server.public_base_url 必须为 http(s) URL: {}",
                base
            );
            anyhow::ensure!(
                parsed.host_str().is_some(),
                "server.public_base_url 缺少主机名: {}",
                base
            );
        }

        // Session 校验
        let valid_same_site = ["Strict", "Lax", "None"];
        anyhow::ensure!(
            valid_same_site.contains(&self.session.same_site.as_str()),
            "session.same_site 无效: {}（期望 Strict/Lax/None）",
            self.session.same_site
        );

        // Admin 校验
        if self.admin.auth_mode == "token" {
            anyhow::ensure!(
                !self.admin.auth_token.is_empty(),
                "admin.auth_token 不能为空（auth_mode = token）"
            );
        }

        // 生产环境防呆（BFF_ENV=prod 时拒绝 POC 配置）
        if is_prod {
            for (label, kind) in [
                ("session_store", &self.provider.session_store),
                ("cache", &self.provider.cache),
                ("lock", &self.provider.lock),
            ] {
                anyhow::ensure!(
                    kind != "memory",
                    "生产环境（BFF_ENV=prod）不允许 provider.{}=memory（无法多实例、重启丢状态）",
                    label
                );
            }
            anyhow::ensure!(
                self.admin.auth_mode == "token",
                "生产环境（BFF_ENV=prod）不允许 admin.auth_mode=none（管理面将无鉴权）"
            );
            anyhow::ensure!(
                self.admin.auth_token != "changeme" && self.admin.auth_token.len() >= 32,
                "生产环境（BFF_ENV=prod）admin.auth_token 必须为 ≥32 字符的随机值（当前为默认弱口令或过短）"
            );
            anyhow::ensure!(
                !self.admin.enable_test_endpoints,
                "生产环境（BFF_ENV=prod）必须设置 admin.enable_test_endpoints=false"
            );
            for p in &self.oidc.providers {
                anyhow::ensure!(
                    !p.insecure_skip_id_token_verification,
                    "生产环境（BFF_ENV=prod）不允许 oidc.providers[{}].insecure_skip_id_token_verification=true",
                    p.id
                );
            }
            anyhow::ensure!(
                self.session.secure,
                "生产环境（BFF_ENV=prod）必须 session.secure=true（否则会话 Cookie 明文传输）"
            );
            // 显式多站点由每站点 public_base_url 承担防污染要求（validate_sites 校验，
            // 且顶层 server.public_base_url / trusted_hosts 此时为 dead config）；legacy 语义不变。
            anyhow::ensure!(
                !self.sites.is_empty()
                    || self.server.public_base_url.is_some()
                    || !self.server.trusted_hosts.is_empty(),
                "生产环境（BFF_ENV=prod）必须配置 server.public_base_url 或 server.trusted_hosts（防止 Host 头污染 redirect_uri）"
            );
            anyhow::ensure!(
                self.bff_secret.secret != "change-me-in-production"
                    && self.bff_secret.salt != "default-salt-at-least-16-bytes",
                "生产环境（BFF_ENV=prod）必须通过 BFF_SECRET / BFF_SECRET_SALT 注入真实主密钥"
            );
        }

        // 限流参数校验
        anyhow::ensure!(
            self.rate_limit.per_second > 0,
            "rate_limit.per_second 必须 > 0"
        );

        // OTel 遥测配置：endpoint 非空时必须为合法 http(s) URL
        if let Some(endpoint) = &self.telemetry.otlp_endpoint {
            let parsed = url::Url::parse(endpoint)
                .map_err(|e| anyhow::anyhow!("telemetry.otlp_endpoint 非法: {}", e))?;
            anyhow::ensure!(
                parsed.scheme() == "http" || parsed.scheme() == "https",
                "telemetry.otlp_endpoint 必须为 http(s) URL: {}",
                endpoint
            );
            anyhow::ensure!(
                parsed.host_str().is_some(),
                "telemetry.otlp_endpoint 缺少主机名: {}",
                endpoint
            );
        }
        anyhow::ensure!(
            !self.telemetry.sample_ratio.is_nan()
                && (0.0..=1.0).contains(&self.telemetry.sample_ratio),
            "telemetry.sample_ratio 必须在 0.0..=1.0 之间: {}",
            self.telemetry.sample_ratio
        );

        // 认证端点 per-IP 限流校验
        if self.auth_rate_limit.enabled {
            anyhow::ensure!(
                self.auth_rate_limit.per_ip.per_second > 0,
                "auth_rate_limit.per_ip.per_second 必须 > 0"
            );
            anyhow::ensure!(
                self.auth_rate_limit.per_ip.burst_size > 0,
                "auth_rate_limit.per_ip.burst_size 必须 > 0"
            );
            anyhow::ensure!(
                !self.auth_rate_limit.paths.is_empty(),
                "auth_rate_limit.enabled = true 时 paths 不能为空"
            );
            for p in &self.auth_rate_limit.paths {
                anyhow::ensure!(
                    p.starts_with('/'),
                    "auth_rate_limit.paths 条目必须以 / 开头: {}",
                    p
                );
            }
        }

        // OIDC provider 校验
        for p in &self.oidc.providers {
            anyhow::ensure!(!p.id.is_empty(), "OIDC provider id 不能为空");
            anyhow::ensure!(
                url::Url::parse(&p.issuer_url).is_ok(),
                "OIDC provider {} issuer_url 非法",
                p.id
            );
            anyhow::ensure!(
                !p.client_id.is_empty(),
                "OIDC provider {} client_id 不能为空",
                p.id
            );
            // callback_path 用于注册回调路由，必须是可用且不冲突的绝对路径
            anyhow::ensure!(
                p.callback_path.starts_with('/'),
                "OIDC provider {} callback_path 必须以 / 开头: {}",
                p.id,
                p.callback_path
            );
            const RESERVED: &[&str] = &["/login", "/logout", "/live", "/ready", "/ws", "/pipeline"];
            anyhow::ensure!(
                !RESERVED
                    .iter()
                    .any(|r| p.callback_path == *r
                        || p.callback_path.starts_with(&format!("{}/", r))),
                "OIDC provider {} callback_path 与保留路径冲突: {}",
                p.id,
                p.callback_path
            );
        }

        // Pipeline 校验
        for (name, def) in &self.pipelines {
            crate::orchestration::dag::validate_pipeline(name, def)?;
            // 校验 step timeout 范围
            for step in &def.steps {
                if let Some(t) = step.config.timeout {
                    anyhow::ensure!(
                        t <= Duration::from_secs(300),
                        "pipeline [{}] step [{}] timeout 不能超过 300s",
                        name,
                        step.id
                    );
                }
            }
        }

        // Route 校验
        for (i, route) in self.routes.iter().enumerate() {
            anyhow::ensure!(
                route.path.starts_with('/'),
                "routes[{}] path 必须以 / 开头: {}",
                i,
                route.path
            );
            if route.route_type == crate::config::RouteType::Proxy {
                anyhow::ensure!(
                    route.config.upstream.is_some(),
                    "routes[{}] proxy 类型必须配置 upstream",
                    i
                );
                if let Some(ref u) = route.config.upstream {
                    anyhow::ensure!(
                        url::Url::parse(u).is_ok(),
                        "routes[{}] upstream URL 非法: {}",
                        i,
                        u
                    );
                }
            }

            // Token Exchange 校验（§4.4）
            if let Some(te) = &route.config.token_exchange {
                if let Some(ep) = &te.token_endpoint {
                    anyhow::ensure!(
                        url::Url::parse(ep).is_ok(),
                        "routes[{}] token_exchange.token_endpoint URL 非法: {}",
                        i,
                        ep
                    );
                }
                anyhow::ensure!(
                    !te.client_id.is_empty(),
                    "routes[{}] token_exchange.client_id 不能为空",
                    i
                );
                anyhow::ensure!(
                    te.cache_ttl > Duration::ZERO,
                    "routes[{}] token_exchange.cache_ttl 必须 > 0",
                    i
                );
                for (label, val) in [
                    ("subject_token_type", &te.subject_token_type),
                    ("requested_token_type", &te.requested_token_type),
                ] {
                    anyhow::ensure!(
                        val.starts_with("urn:ietf:params:oauth:token-type:"),
                        "routes[{}] token_exchange.{} 必须为合法的 token-type URN 前缀: {}",
                        i,
                        label,
                        val
                    );
                }
                // 交换以会话 access token 为 subject_token，必须开启认证
                anyhow::ensure!(
                    route.auth_required,
                    "routes[{}] 配置 token_exchange 要求 auth_required: true",
                    i
                );
                // WS 路径本期不执行交换（§5.4），仅告警
                if route.config.proxy_mode == "websocket" {
                    tracing::warn!(
                        "routes[{}] WebSocket 路由上的 token_exchange 本期不生效（保持直接注入会话 token）",
                        i
                    );
                }
                // 无 audience/scope 时交换无收窄效果，仅告警
                if te.audience.is_empty() && te.scope.is_empty() {
                    tracing::warn!(
                        "routes[{}] token_exchange 未配置 audience/scope，交换无收窄效果",
                        i
                    );
                }
                // actor_token 委托为设计预留：本期无值来源，配置后不生效（§4.2），仅告警
                if !te.actor_token_type.is_empty() {
                    tracing::warn!(
                        "routes[{}] token_exchange.actor_token_type 为预留字段，本期不生效",
                        i
                    );
                }
            }
        }

        // 熔断器校验
        anyhow::ensure!(
            self.circuit_breaker.failure_threshold > 0,
            "circuit_breaker.failure_threshold 必须 > 0"
        );

        // 脚本超时校验
        anyhow::ensure!(
            self.scripting.max_duration <= Duration::from_secs(30),
            "scripting.max_duration 不能超过 30s"
        );

        // 请求体限制校验
        anyhow::ensure!(
            self.body_limit.max_bytes <= 100 * 1024 * 1024,
            "body_limit.max_bytes 不能超过 100 MiB"
        );

        // 健康检查校验
        for (i, u) in self.health.upstreams.iter().enumerate() {
            anyhow::ensure!(
                url::Url::parse(u).is_ok(),
                "health.upstreams[{}] URL 非法: {}",
                i,
                u
            );
        }
        anyhow::ensure!(
            self.health.probe_timeout >= Duration::from_millis(100),
            "health.probe_timeout 必须 >= 100ms"
        );

        // 会话 profile 卫生与 Cookie 策略（§5.4 第 3–5 条）与站点字段/Host 白名单
        // （§5.4 第 1/2/6/7/11 条）：在现有 legacy 全局校验之后执行。
        let sites = self.effective_sites();
        self.validate_session_profiles(is_prod, &sites)?;
        self.validate_sites(is_prod, &sites)?;

        Ok(())
    }

    /// 会话 profile 卫生与 Cookie 策略校验（§5.4 第 3–5 条）。
    ///
    /// - 规则 3（无条件）：profile 名合法且非 `default`；`cookie_name` 全局唯一；
    ///   `same_site ∈ {Strict, Lax, None}` 且 `None ⇒ secure`；prod 逐 profile 校验 `secure`；
    /// - 规则 4（仅被站点引用的 profile）：`cookie_domain` 非空必须 domain-match
    ///   引用它的全部站点主机；被 ≥2 个不同主机站点引用且为空 → prod 失败、dev 告警；
    /// - 规则 5：prod 下任一 profile `cookie_domain` 非空且未显式
    ///   `allow_unmanaged_subdomains: true` → 失败；dev 告警。
    fn validate_session_profiles(
        &self,
        is_prod: bool,
        sites: &[ResolvedSite],
    ) -> anyhow::Result<()> {
        // 规则 1：`resolved_session_profiles` 会跳过 `default` 键，故直接检查原始键。
        for name in self.session_profiles.keys() {
            anyhow::ensure!(
                is_valid_profile_name(name),
                "session_profiles[{}] 名称非法（须匹配 [a-z0-9-]+）",
                name
            );
            anyhow::ensure!(
                name != DEFAULT_SESSION_PROFILE,
                "session_profiles[{}] 不得定义：`default` 只能由顶层 `session` 定义",
                name
            );
        }

        // 解析后的 profile 按固定顺序处理（default 优先 + 其余字典序），保证错误可复现。
        let profiles = self.resolved_session_profiles();
        let mut names = vec![DEFAULT_SESSION_PROFILE.to_string()];
        let mut extras: Vec<String> = profiles
            .keys()
            .filter(|n| n.as_str() != DEFAULT_SESSION_PROFILE)
            .cloned()
            .collect();
        extras.sort();
        names.extend(extras);

        // 规则 2/3/4：cookie_name 唯一、same_site 取值、prod 逐 profile secure。
        let valid_same_site = ["Strict", "Lax", "None"];
        let mut seen_cookie_names: HashMap<&str, &str> = HashMap::new();
        for name in &names {
            let profile = profiles
                .get(name)
                .expect("default 与 session_profiles 解析后必存在");

            if let Some(prev) = seen_cookie_names.insert(&profile.cookie_name, name) {
                anyhow::bail!(
                    "{} 必须全局唯一，与 profile [{}] 冲突: {}",
                    profile_field_path(name, "cookie_name"),
                    prev,
                    profile.cookie_name
                );
            }
            anyhow::ensure!(
                valid_same_site.contains(&profile.same_site.as_str()),
                "{} 无效: {}（期望 Strict/Lax/None）",
                profile_field_path(name, "same_site"),
                profile.same_site
            );
            anyhow::ensure!(
                profile.same_site != "None" || profile.secure,
                "{} 为 None 时必须 secure: true（否则浏览器拒绝发送跨站 Cookie）",
                profile_field_path(name, "same_site")
            );
            if is_prod {
                anyhow::ensure!(
                    profile.secure,
                    "生产环境（BFF_ENV=prod）必须 {} 为 true（否则会话 Cookie 明文传输）",
                    profile_field_path(name, "secure")
                );
            }

            // 规则 5：共享域信任边界显式确认（与是否被站点引用无关）。
            let has_domain = profile
                .cookie_domain
                .as_deref()
                .filter(|d| !d.is_empty())
                .is_some();
            if has_domain && !profile.allow_unmanaged_subdomains {
                if is_prod {
                    anyhow::bail!(
                        "生产环境（BFF_ENV=prod）下 {} 非空必须显式 {}: true（确认该域内未托管子域也会收到会话 Cookie）",
                        profile_field_path(name, "cookie_domain"),
                        profile_field_path(name, "allow_unmanaged_subdomains")
                    );
                }
                tracing::warn!(
                    "{} 非空但未确认 {}: 该域内未托管子域也会收到会话 Cookie（dev 仅告警）",
                    profile_field_path(name, "cookie_domain"),
                    profile_field_path(name, "allow_unmanaged_subdomains")
                );
            }
        }

        // 规则 4/5：cookie_domain 与引用站点主机的匹配、共享域确认。
        for name in &names {
            let profile = profiles
                .get(name)
                .expect("default 与 session_profiles 解析后必存在");
            let referencing: Vec<&ResolvedSite> = sites
                .iter()
                .filter(|s| s.session_profile == name.as_str())
                .collect();
            if referencing.is_empty() {
                continue;
            }

            match profile.cookie_domain.as_deref().filter(|d| !d.is_empty()) {
                Some(domain) => {
                    // 规则 4：正向 domain-match 引用该 profile 的每个站点主机。
                    for site in &referencing {
                        for host in &site.allowed_hosts {
                            anyhow::ensure!(
                                cookie_domain_matches(domain, host),
                                "{} ({}) 不匹配引用站点 [{}] 的主机 {}（须为该主机的父域）",
                                profile_field_path(name, "cookie_domain"),
                                domain,
                                site.name,
                                host
                            );
                        }
                    }
                }
                None => {
                    // cookie_domain 为空（host-only）时无法跨主机共享会话。
                    let mut hosts: Vec<&str> = Vec::new();
                    for site in &referencing {
                        for host in &site.allowed_hosts {
                            if !hosts.contains(&host.as_str()) {
                                hosts.push(host);
                            }
                        }
                    }
                    if referencing.len() >= 2 && hosts.len() > 1 {
                        if is_prod {
                            anyhow::bail!(
                                "{} 为空但被 {} 个不同主机站点引用（无法跨主机共享会话，须配置 Domain cookie）",
                                profile_field_path(name, "cookie_domain"),
                                hosts.len()
                            );
                        }
                        tracing::warn!(
                            "{} 为空但被 {} 个不同主机站点引用（无法跨主机共享会话，prod 将拒绝启动）",
                            profile_field_path(name, "cookie_domain"),
                            hosts.len()
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// 站点字段与 Host 白名单校验（§5.4 第 1/2/6/7/11 条 + §5.3 重复路由告警）。
    ///
    /// - 规则 1/2/3：站点名合法（`[a-z0-9-]+`）且唯一、保留名 `admin` 禁止；端口非 0、
    ///   唯一、≠ `admin_port`；`bind` 必须为可解析 IP；`session_profile` 必须已定义；
    /// - 规则 6（prod）：每站点 `public_base_url` 必填、`server_names` 配置时必须包含 public 主机、
    ///   站点间归一化后唯一；显式多站点下顶层 `server.public_base_url` / `trusted_hosts` 为
    ///   dead config（error）；`server.business_port` 被忽略（≠ 8080 时 warn）；
    /// - 规则 7：`server_names` 仅主机名（复用 `normalize_host`，读原始值以免非法项被静默丢弃），
    ///   归一化后站点内唯一；
    /// - 规则 11：prod 下多站点复用同一 `spa.dir` 仅告警；
    /// - legacy 冻结：无 `sites` 定义时禁止 `routes[].sites` 注解（路由会静默不可命中）。
    fn validate_sites(&self, is_prod: bool, sites: &[ResolvedSite]) -> anyhow::Result<()> {
        // legacy 分支：合成 default 站点，站点级字段无显式配置；仅冻结 routes[].sites 语义。
        if self.sites.is_empty() {
            for (i, route) in self.routes.iter().enumerate() {
                anyhow::ensure!(
                    route.sites.is_empty(),
                    "routes[{}].sites 在未定义 sites 的 legacy 配置中不可用（路由将不可命中；请定义 sites 或移除注解）",
                    i
                );
            }
            self.warn_duplicate_routes(&["default".to_string()]);
            return Ok(());
        }

        // 显式多站点下顶层 Host 配置不再生效（dead config），避免“配置了但行为不变”。
        anyhow::ensure!(
            self.server.public_base_url.is_none(),
            "显式多站点下 server.public_base_url 为 dead config（每站点改用 sites[i].public_base_url）"
        );
        anyhow::ensure!(
            self.server.trusted_hosts.is_empty(),
            "显式多站点下 server.trusted_hosts 为 dead config（每站点改用 sites[i].server_names）"
        );
        if self.server.business_port != default_business_port() {
            tracing::warn!(
                "显式多站点下 server.business_port={} 被忽略（每站点使用 sites[].port）",
                self.server.business_port
            );
        }

        let profiles = self.resolved_session_profiles();
        let mut seen_names: HashMap<&str, usize> = HashMap::new();
        let mut seen_ports: HashMap<u16, usize> = HashMap::new();
        let mut seen_public_urls: HashMap<String, usize> = HashMap::new();

        for (i, raw) in self.sites.iter().enumerate() {
            // 规则 1：站点名非空、非保留名、字符集、唯一。
            anyhow::ensure!(!raw.name.is_empty(), "sites[{}].name 不能为空", i);
            anyhow::ensure!(
                raw.name != "admin",
                "sites[{}].name 为保留名 admin（禁止与指标/日志/管理面命名空间混淆）",
                i
            );
            anyhow::ensure!(
                is_valid_site_name(&raw.name),
                "sites[{}].name 非法（须匹配 [a-z0-9-]+）: {}",
                i,
                raw.name
            );
            if let Some(prev) = seen_names.insert(raw.name.as_str(), i) {
                anyhow::bail!(
                    "sites[{}].name 重复，与 sites[{}] 冲突: {}",
                    i,
                    prev,
                    raw.name
                );
            }

            // 规则 2：端口非 0、≠ admin_port、唯一。
            anyhow::ensure!(raw.port != 0, "sites[{}].port 不能为 0", i);
            anyhow::ensure!(
                raw.port != self.server.admin_port,
                "sites[{}].port 不能等于 server.admin_port（{}）",
                i,
                self.server.admin_port
            );
            if let Some(prev) = seen_ports.insert(raw.port, i) {
                anyhow::bail!(
                    "sites[{}].port 重复，与 sites[{}] 冲突: {}",
                    i,
                    prev,
                    raw.port
                );
            }

            // bind 仅用于监听，必须是可解析 IP（主机名/通配域不参与 bind）。
            anyhow::ensure!(
                raw.bind.parse::<std::net::IpAddr>().is_ok(),
                "sites[{}].bind 必须为可解析的 IP 地址: {}",
                i,
                raw.bind
            );

            // 规则 7：server_names 仅主机名——校验原始条目，非法值不得被 effective_sites 静默丢弃。
            let mut server_names: Vec<String> = Vec::new();
            for name in &raw.server_names {
                let host = normalize_host(name).ok_or_else(|| {
                    anyhow::anyhow!(
                        "sites[{}].server_names 条目非法（仅允许主机名，拒绝协议/端口/通配符/路径）: {}",
                        i,
                        name
                    )
                })?;
                anyhow::ensure!(
                    !server_names.contains(&host),
                    "sites[{}].server_names 条目重复（归一化后）: {}",
                    i,
                    host
                );
                server_names.push(host);
            }

            // 规则 6：public_base_url 合法性（http/https、无 path/query）与站点间归一化唯一。
            let public_host = match &raw.public_base_url {
                Some(raw_url) => {
                    let (normalized, host) = normalize_public_base_url(raw_url)
                        .map_err(|e| anyhow::anyhow!("sites[{}].public_base_url 非法: {}", i, e))?;
                    if let Some(prev) = seen_public_urls.insert(normalized.clone(), i) {
                        anyhow::bail!(
                            "sites[{}].public_base_url 与 sites[{}] 归一化后重复: {}",
                            i,
                            prev,
                            normalized
                        );
                    }
                    Some(host)
                }
                None => None,
            };

            // 规则 3：session_profile 必须引用已解析的 profile（`default` = 顶层 `session`）。
            anyhow::ensure!(
                profiles.contains_key(&raw.session_profile),
                "sites[{}].session_profile 引用未定义的 profile: {}",
                i,
                raw.session_profile
            );

            // 规则 6（prod）：每站点 public_base_url 必填；server_names 配置时必须包含 public 主机。
            if is_prod {
                let host = public_host.as_deref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "生产环境（BFF_ENV=prod）下 sites[{}].public_base_url 必填（redirect_uri 一律由此推导）",
                        i
                    )
                })?;
                if !server_names.is_empty() {
                    anyhow::ensure!(
                        server_names.iter().any(|h| h == host),
                        "sites[{}].server_names 必须包含 public_base_url 主机 {}",
                        i,
                        host
                    );
                }
            }
        }

        // 规则 11：prod 下多站点复用同一 spa.dir 仅告警（可能误配，也可能有意复用）。
        if is_prod {
            let mut seen_spa: HashMap<&str, usize> = HashMap::new();
            for (i, site) in sites.iter().enumerate() {
                if let Some(prev) = seen_spa.insert(site.spa_dir.as_str(), i) {
                    tracing::warn!(
                        "sites[{}].spa.dir 与 sites[{}] 相同（{}）：prod 下多站点复用同一 SPA 目录可能误配",
                        i,
                        prev,
                        site.spa_dir
                    );
                }
            }
        }

        let site_names: Vec<String> = self.sites.iter().map(|s| s.name.clone()).collect();
        self.warn_duplicate_routes(&site_names);
        Ok(())
    }

    /// §5.3：对同一站点可命中的完全同规格路由（相同归一化 path + methods + 站点范围）告警。
    ///
    /// 仅 warn 不 fail（保持 legacy 兼容）；重复路由在运行期仍按配置顺序 last-wins。
    /// `all_sites` 为全部已定义站点名，用于把「未标注 sites = 全部站点」展开为可比较集合。
    fn warn_duplicate_routes(&self, all_sites: &[String]) {
        let mut seen: HashMap<(String, Vec<String>, Vec<String>), usize> = HashMap::new();
        for (i, route) in self.routes.iter().enumerate() {
            let mut methods: Vec<String> = route
                .methods
                .iter()
                .map(|m| m.to_ascii_uppercase())
                .collect();
            methods.sort();
            methods.dedup();

            let mut scope: Vec<String> = if route.sites.is_empty() {
                all_sites.to_vec()
            } else {
                route.sites.clone()
            };
            scope.sort();
            scope.dedup();

            let key = (normalize_route_path(&route.path), methods, scope);
            if let Some(prev) = seen.insert(key, i) {
                tracing::warn!(
                    "routes[{}] 与 routes[{}] 规格相同且对同一组站点生效，后者按配置顺序 last-wins",
                    i,
                    prev
                );
            }
        }
    }

    /// 脱敏副本：隐藏 bff_secret 主密钥、各 client_secret、管理 token、
    /// 含凭据的 Redis URL、TLS 私钥路径，用于导出。
    pub fn sanitized(&self) -> Self {
        let mut c = self.clone();
        // 主密钥绝不能出现在导出结果中——结合会话数据可解密全部用户令牌
        if !c.bff_secret.secret.is_empty() {
            c.bff_secret.secret = SECRET_SENTINEL.into();
        }
        if !c.bff_secret.salt.is_empty() {
            c.bff_secret.salt = SECRET_SENTINEL.into();
        }
        // Redis URL 含凭据（user:pass@host）时整体打码
        if c.provider.redis_url.contains('@') {
            c.provider.redis_url = SECRET_SENTINEL.into();
        }
        if c.http_client.client_key_path.is_some() {
            c.http_client.client_key_path = Some(SECRET_SENTINEL.into());
        }
        for p in &mut c.oidc.providers {
            if !p.client_secret.is_empty() {
                p.client_secret = SECRET_SENTINEL.into();
            }
        }
        if !c.admin.auth_token.is_empty() {
            c.admin.auth_token = SECRET_SENTINEL.into();
        }
        for r in &mut c.routes {
            if let Some(te) = &mut r.config.token_exchange {
                if !te.client_secret.is_empty() {
                    te.client_secret = SECRET_SENTINEL.into();
                }
            }
        }
        c
    }

    /// 导入时合并敏感信息：识别 `***` 哨兵并从现有配置回填真实值（保留已注入的环境值）。
    ///
    /// 规则：导出→导入回环不得破坏任何密钥，覆盖
    /// `bff_secret.{secret,salt}`、`provider.redis_url`、TLS 私钥路径、
    /// `admin.auth_token`、`oidc.providers[].client_secret`（按 id 对齐）、
    /// `token_exchange.client_secret`（按 route.path 对齐）。
    pub fn merge_sensitive_secrets(&mut self, existing: &AppConfig) {
        if self.bff_secret.secret == SECRET_SENTINEL {
            self.bff_secret.secret = existing.bff_secret.secret.clone();
        }
        if self.bff_secret.salt == SECRET_SENTINEL {
            self.bff_secret.salt = existing.bff_secret.salt.clone();
        }
        if self.provider.redis_url == SECRET_SENTINEL {
            self.provider.redis_url = existing.provider.redis_url.clone();
        }
        if self.http_client.client_key_path.as_deref() == Some(SECRET_SENTINEL) {
            self.http_client.client_key_path = existing.http_client.client_key_path.clone();
        }
        if self.admin.auth_token == SECRET_SENTINEL {
            self.admin.auth_token = existing.admin.auth_token.clone();
        }
        for p in &mut self.oidc.providers {
            if p.client_secret != SECRET_SENTINEL {
                continue;
            }
            if let Some(ex) = existing.oidc.providers.iter().find(|e| e.id == p.id) {
                p.client_secret = ex.client_secret.clone();
            }
        }
        for route in &mut self.routes {
            let Some(te) = &mut route.config.token_exchange else {
                continue;
            };
            if te.client_secret != SECRET_SENTINEL {
                continue;
            }
            if let Some(ex_route) = existing
                .routes
                .iter()
                .find(|r| r.path == route.path && r.config.token_exchange.is_some())
            {
                if let Some(ex_te) = &ex_route.config.token_exchange {
                    if ex_te.client_secret != SECRET_SENTINEL {
                        te.client_secret = ex_te.client_secret.clone();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_secrets() -> AppConfig {
        serde_yaml::from_str(
            r#"
bff_secret:
  secret: "real-master-secret-123"
  salt: "real-master-salt-456"
admin:
  auth_token: "real-admin-token"
provider:
  redis_url: "redis://:redis-password@localhost:6379"
http_client:
  client_key_path: "/etc/bff/client.key"
oidc:
  providers:
    - id: p1
      issuer_url: "https://idp.example.com"
      client_id: "cid"
      client_secret: "real-oidc-secret"
routes:
  - path: "/api/exchange"
    type: proxy
    config:
      upstream: "https://upstream.example.com"
      token_exchange:
        client_id: "te-client"
        client_secret: "real-te-secret"
"#,
        )
        .expect("测试配置解析失败")
    }

    /// 脱敏必须覆盖全部敏感字段，且序列化结果不含任何真实值。
    #[test]
    fn sanitized_hides_all_secrets() {
        let cfg = config_with_secrets();
        let s = cfg.sanitized();
        assert_eq!(s.bff_secret.secret, SECRET_SENTINEL);
        assert_eq!(s.bff_secret.salt, SECRET_SENTINEL);
        assert_eq!(s.admin.auth_token, SECRET_SENTINEL);
        assert_eq!(s.provider.redis_url, SECRET_SENTINEL);
        assert_eq!(
            s.http_client.client_key_path.as_deref(),
            Some(SECRET_SENTINEL)
        );
        assert_eq!(s.oidc.providers[0].client_secret, SECRET_SENTINEL);
        assert_eq!(
            s.routes[0]
                .config
                .token_exchange
                .as_ref()
                .unwrap()
                .client_secret,
            SECRET_SENTINEL
        );

        let yaml = serde_yaml::to_string(&s).unwrap();
        for secret in [
            "real-master-secret-123",
            "real-master-salt-456",
            "real-admin-token",
            "redis-password",
            "real-oidc-secret",
            "real-te-secret",
            "/etc/bff/client.key",
        ] {
            assert!(!yaml.contains(secret), "导出结果泄露敏感值: {}", secret);
        }
    }

    /// 导出→导入回环必须完整恢复所有密钥（不得覆盖为 `***`）。
    #[test]
    fn export_import_roundtrip_preserves_secrets() {
        let original = config_with_secrets();
        let mut imported = original.sanitized();
        imported.merge_sensitive_secrets(&original);

        assert_eq!(imported.bff_secret.secret, "real-master-secret-123");
        assert_eq!(imported.bff_secret.salt, "real-master-salt-456");
        assert_eq!(imported.admin.auth_token, "real-admin-token");
        assert_eq!(
            imported.provider.redis_url,
            "redis://:redis-password@localhost:6379"
        );
        assert_eq!(
            imported.http_client.client_key_path.as_deref(),
            Some("/etc/bff/client.key")
        );
        assert_eq!(imported.oidc.providers[0].client_secret, "real-oidc-secret");
        assert_eq!(
            imported.routes[0]
                .config
                .token_exchange
                .as_ref()
                .unwrap()
                .client_secret,
            "real-te-secret"
        );
    }

    /// 非哨兵值（真实新密钥）不得被回填覆盖——是否可用由 replace_config 决定。
    #[test]
    fn merge_leaves_non_sentinel_values_intact() {
        let original = config_with_secrets();
        let mut imported: AppConfig = serde_yaml::from_str(
            r#"
bff_secret:
  secret: "brand-new-secret"
  salt: "brand-new-salt-value"
"#,
        )
        .unwrap();
        imported.merge_sensitive_secrets(&original);
        assert_eq!(imported.bff_secret.secret, "brand-new-secret");
        assert_eq!(imported.bff_secret.salt, "brand-new-salt-value");
    }
}

// ── 多站点配置测试（模型 / 归一化 / legacy 合成）──

#[cfg(test)]
mod multi_site_config_tests {
    use super::*;

    /// §5.1 / 附录 B 两站点样例。
    const MULTISITE_YAML: &str = r#"
server:
  admin_port: 8443
sites:
  - name: app1
    port: 8081
    bind: "0.0.0.0"
    server_names: ["app1.example.com"]
    public_base_url: "https://app1.example.com"
    session_profile: default
    spa: { dir: "apps/app1/dist" }
    oidc:
      default_provider: app1
      allowed_providers: [app1]
    logout_scope: global
  - name: app2
    port: 8082
    server_names: ["app2.example.com"]
    public_base_url: "https://app2.example.com"
    session_profile: default
    spa: { dir: "apps/app2/dist" }
    oidc:
      default_provider: app2
      allowed_providers: [app2]
    logout_scope: global
session:
  cookie_name: "BFF_SESSION_V2"
  cookie_domain: ".example.com"
  secure: true
  http_only: true
  same_site: "Lax"
  ttl: "336h"
  allow_unmanaged_subdomains: true
oidc:
  providers:
    - id: app1
      display_name: "App1"
      issuer_url: "https://idp.example.com"
      client_id: "app1-client"
      client_secret: "${APP1_SECRET}"
      callback_path: "/auth/callback"
      shared_across_sites: false
    - id: app2
      display_name: "App2"
      issuer_url: "https://idp.example.com"
      client_id: "app2-client"
      client_secret: "${APP2_SECRET}"
      callback_path: "/auth/callback"
      shared_across_sites: false
"#;

    #[test]
    fn parses_explicit_multisite_and_resolves_paths() {
        let cfg: AppConfig = serde_yaml::from_str(MULTISITE_YAML).unwrap();
        let sites = cfg.effective_sites();
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0].name, "app1");
        assert_eq!(sites[0].port, 8081);
        assert_eq!(sites[0].allowed_hosts, vec!["app1.example.com"]);
        assert_eq!(sites[0].session_profile, "default");
        assert_eq!(sites[0].spa_dir, "apps/app1/dist");
        assert_eq!(sites[0].allowed_providers, vec!["app1"]);
        assert!(!sites[0].legacy);
    }

    #[test]
    fn legacy_config_synthesizes_default_site() {
        let cfg = AppConfig::default();
        let sites = cfg.effective_sites();
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].name, "default");
        assert_eq!(sites[0].port, cfg.server.business_port);
        assert!(sites[0].legacy);
    }

    #[test]
    fn session_profile_inheritance_and_explicit_empty_domain() {
        let cfg: AppConfig = serde_yaml::from_str(
            r#"
session:
  cookie_name: "A"
  cookie_domain: ".Example.com"
  secure: true
  same_site: "Lax"
session_profiles:
  isolated:
    cookie_name: "B"
    cookie_domain: ""
"#,
        )
        .unwrap();
        let p = cfg.resolved_session_profiles();
        assert_eq!(p["default"].cookie_domain.as_deref(), Some("example.com"));
        assert_eq!(p["isolated"].cookie_domain, None); // 显式空串 = 强制 host-only
        assert!(p["isolated"].secure); // 继承顶层
    }

    #[test]
    fn normalizes_hosts() {
        assert_eq!(
            normalize_host(" App1.EXAMPLE.com. "),
            Some("app1.example.com".to_string())
        );
        assert_eq!(normalize_host("[::1]"), Some("::1".to_string()));
        assert_eq!(normalize_host("example.com:8080"), None);
        assert_eq!(normalize_host("https://example.com"), None);
        assert_eq!(normalize_host("*.example.com"), None);
        assert_eq!(normalize_host("a/b"), None);
        assert_eq!(normalize_host("   "), None);
        assert_eq!(normalize_host(""), None);
        assert_eq!(normalize_host("bad host"), None);
    }

    #[test]
    fn normalizes_cookie_domains() {
        assert_eq!(
            normalize_cookie_domain(".Example.com"),
            Some("example.com".to_string())
        );
        assert_eq!(normalize_cookie_domain(""), Some(String::new()));
        assert_eq!(normalize_cookie_domain("bad domain"), None);
    }

    #[test]
    fn matches_cookie_domains_only_on_label_boundaries() {
        assert!(cookie_domain_matches("example.com", "example.com"));
        assert!(cookie_domain_matches("example.com", "app1.example.com"));
        assert!(!cookie_domain_matches("example.com", "evilexample.com"));
        assert!(!cookie_domain_matches("example.com", "example.com.evil"));
        assert!(!cookie_domain_matches("", "example.com"));
    }

    #[test]
    fn normalizes_public_base_url() {
        assert_eq!(
            normalize_public_base_url("https://app1.example.com/").unwrap(),
            (
                "https://app1.example.com".to_string(),
                "app1.example.com".to_string()
            )
        );
        assert_eq!(
            normalize_public_base_url("http://127.0.0.1:8081")
                .unwrap()
                .1,
            "127.0.0.1"
        );
        assert!(normalize_public_base_url("https://app1.example.com/path").is_err());
        assert!(normalize_public_base_url("https://app1.example.com/?a=1").is_err());
        assert!(normalize_public_base_url("https://app1.example.com/#frag").is_err());
        assert!(normalize_public_base_url("ftp://app1.example.com").is_err());
        assert!(normalize_public_base_url("not-a-url").is_err());
    }

    #[test]
    fn explicit_site_inherits_global_spa_and_provider_defaults() {
        let cfg: AppConfig = serde_yaml::from_str(
            r#"
spa: { dir: "global/dist" }
sites:
  - name: app1
    port: 8081
    oidc: { default_provider: p1 }
oidc:
  providers:
    - id: p1
      issuer_url: "https://idp.example.com"
      client_id: "cid"
"#,
        )
        .unwrap();
        let sites = cfg.effective_sites();
        assert_eq!(sites[0].spa_dir, "global/dist");
        assert_eq!(sites[0].session_profile, "default");
        assert_eq!(sites[0].bind, "0.0.0.0");
        assert_eq!(sites[0].allowed_providers, vec!["p1"]);
        assert!(sites[0].allowed_hosts.is_empty());
        assert_eq!(sites[0].public_base_url, None);
        assert_eq!(
            cfg.resolved_session_profiles()["default"].cookie_domain,
            None
        );
    }

    /// legacy 配置导出必须与升级前完全一致：新增字段在默认值下不出现。
    #[test]
    fn legacy_serialization_omits_new_fields() {
        let cfg: AppConfig = serde_yaml::from_str(
            r#"
session:
  secure: true
oidc:
  providers:
    - id: p1
      issuer_url: "https://idp.example.com"
      client_id: "cid"
routes:
  - path: "/api/x"
    type: static
    config: { status: 200 }
"#,
        )
        .unwrap();
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        for key in [
            "sites",
            "session_profiles",
            "enforce_host",
            "cookie_domain",
            "allow_unmanaged_subdomains",
            "shared_across_sites",
        ] {
            assert!(
                !yaml.contains(key),
                "legacy 导出不应包含新增字段 {key}:\n{yaml}"
            );
        }
    }

    #[test]
    fn route_sites_field_roundtrips_when_present() {
        let cfg: AppConfig = serde_yaml::from_str(
            r#"
routes:
  - path: "/api/x"
    sites: ["app1", "app2"]
    type: static
    config: { status: 200 }
"#,
        )
        .unwrap();
        assert_eq!(cfg.routes[0].sites, vec!["app1", "app2"]);
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        assert!(yaml.contains("sites"), "带 sites 注解的路由应序列化该字段");
    }
}

// ── session profile 校验测试（§5.4 第 3–5 条）──

#[cfg(test)]
mod session_profile_validation_tests {
    use super::*;

    /// prod 下满足既有防呆校验的最小配置（让新增 profile 校验错误可稳定复现）。
    const PROD_BASE: &str = r#"
bff_secret:
  secret: "prod-real-secret"
  salt: "prod-real-salt-16b"
provider:
  session_store: redis
  cache: redis
  lock: redis
  redis_url: "redis://127.0.0.1:6379"
admin:
  auth_mode: token
  auth_token: "0123456789abcdef0123456789abcdef"
  enable_test_endpoints: false
server:
  public_base_url: "https://bff.example.com"
"#;

    fn prod_yaml(extra: &str) -> String {
        format!("{PROD_BASE}\n{extra}")
    }

    /// prod 下显式多站点用的基础配置：不含顶层 `server.public_base_url` / `trusted_hosts`
    /// （显式多站点下为 dead config，由站点级校验拒绝，改由每站点 `public_base_url` 承担）。
    const PROD_MULTISITE_BASE: &str = r#"
bff_secret:
  secret: "prod-real-secret"
  salt: "prod-real-salt-16b"
provider:
  session_store: redis
  cache: redis
  lock: redis
  redis_url: "redis://127.0.0.1:6379"
admin:
  auth_mode: token
  auth_token: "0123456789abcdef0123456789abcdef"
  enable_test_endpoints: false
session:
  secure: true
"#;

    fn prod_multisite_yaml(extra: &str) -> String {
        format!("{PROD_MULTISITE_BASE}\n{extra}")
    }

    fn expect_err(yaml: &str, need: &str) {
        let cfg: AppConfig = serde_yaml::from_str(yaml).expect("YAML 可解析");
        let err = cfg
            .validate_with_env(false)
            .expect_err("dev 应校验失败")
            .to_string();
        assert!(err.contains(need), "错误应含 {need}，实际: {err}");
    }

    fn expect_ok(yaml: &str) {
        let cfg: AppConfig = serde_yaml::from_str(yaml).expect("YAML 可解析");
        cfg.validate_with_env(false)
            .unwrap_or_else(|e| panic!("dev 应校验通过，实际: {e}"));
    }

    fn expect_prod_err(yaml: &str, need: &str) {
        let cfg: AppConfig = serde_yaml::from_str(yaml).expect("YAML 可解析");
        let err = cfg
            .validate_with_env(true)
            .expect_err("prod 应校验失败")
            .to_string();
        assert!(err.contains(need), "错误应含 {need}，实际: {err}");
    }

    // 规则 1：profile 名 [a-z0-9-]+，且不得为 default。

    #[test]
    fn rejects_invalid_profile_name() {
        expect_err(
            r#"
session_profiles:
  "Bad_Name":
    cookie_name: "B1"
"#,
            "session_profiles[Bad_Name]",
        );
    }

    #[test]
    fn rejects_default_profile_redefinition() {
        expect_err(
            r#"
session_profiles:
  "default":
    cookie_name: "B1"
"#,
            "session_profiles[default]",
        );
    }

    #[test]
    fn accepts_valid_profile_name() {
        expect_ok(
            r#"
session_profiles:
  isolated:
    cookie_name: "B1"
"#,
        );
    }

    // 规则 2：cookie_name 跨全部 profile（含 default）全局唯一。

    #[test]
    fn rejects_duplicate_cookie_name_across_profiles() {
        expect_err(
            r#"
session:
  cookie_name: "BFF_SESSION"
session_profiles:
  isolated:
    cookie_name: "BFF_SESSION"
"#,
            "session_profiles[isolated].cookie_name",
        );
    }

    #[test]
    fn accepts_unique_cookie_names_across_profiles() {
        expect_ok(
            r#"
session:
  cookie_name: "BFF_SESSION"
session_profiles:
  isolated:
    cookie_name: "BFF_SESSION_ISOLATED"
"#,
        );
    }

    // 规则 3：same_site ∈ {Strict, Lax, None}，且 None ⇒ secure: true。

    #[test]
    fn rejects_unknown_same_site_on_extra_profile() {
        expect_err(
            r#"
session_profiles:
  isolated:
    cookie_name: "B1"
    same_site: "Bogus"
"#,
            "session_profiles[isolated].same_site",
        );
    }

    #[test]
    fn rejects_same_site_none_without_secure() {
        expect_err(
            r#"
session:
  secure: false
session_profiles:
  isolated:
    cookie_name: "B1"
    same_site: "None"
"#,
            "same_site",
        );
    }

    #[test]
    fn accepts_same_site_none_with_secure() {
        expect_ok(
            r#"
session:
  secure: true
session_profiles:
  isolated:
    cookie_name: "B1"
    same_site: "None"
"#,
        );
    }

    // 规则 4：prod 逐 profile 校验 secure。

    #[test]
    fn prod_rejects_insecure_extra_profile() {
        expect_prod_err(
            &prod_yaml(
                r#"
session:
  secure: true
session_profiles:
  isolated:
    cookie_name: "B1"
    secure: false
"#,
            ),
            "session_profiles[isolated].secure",
        );
    }

    #[test]
    fn prod_accepts_secure_extra_profile() {
        let cfg: AppConfig = serde_yaml::from_str(&prod_yaml(
            r#"
session:
  secure: true
session_profiles:
  isolated:
    cookie_name: "B1"
    secure: true
"#,
        ))
        .unwrap();
        cfg.validate_with_env(true)
            .unwrap_or_else(|e| panic!("prod 下 secure profile 应通过，实际: {e}"));
    }

    // 规则 4（domain-match）：cookie_domain 非空必须匹配引用该 profile 的全部站点主机。

    #[test]
    fn rejects_cookie_domain_not_matching_referencing_hosts() {
        expect_err(
            r#"
sites:
  - name: app1
    port: 8081
    server_names: ["app1.example.com"]
    session_profile: isolated
session_profiles:
  isolated:
    cookie_name: "B1"
    cookie_domain: ".other.example"
"#,
            "cookie_domain",
        );
    }

    #[test]
    fn accepts_cookie_domain_matching_referencing_hosts() {
        expect_ok(
            r#"
sites:
  - name: app1
    port: 8081
    server_names: ["app1.example.com"]
    session_profile: isolated
session_profiles:
  isolated:
    cookie_name: "B1"
    cookie_domain: "example.com"
"#,
        );
    }

    // 规则 4/5：被 ≥2 个不同主机站点引用且 cookie_domain 为空 → prod 失败、dev 仅告警。

    #[test]
    fn shared_profile_without_domain_fails_prod() {
        let yaml = prod_multisite_yaml(
            r#"
sites:
  - name: app1
    port: 8081
    public_base_url: "https://app1.example.com"
    server_names: ["app1.example.com"]
    session_profile: isolated
  - name: app2
    port: 8082
    public_base_url: "https://app2.example.com"
    server_names: ["app2.example.com"]
    session_profile: isolated
session_profiles:
  isolated:
    cookie_name: "B1"
"#,
        );
        expect_prod_err(&yaml, "cookie_domain");
        let cfg: AppConfig = serde_yaml::from_str(&yaml).unwrap();
        assert!(cfg.validate_with_env(false).is_ok(), "dev 仅告警");
    }

    #[test]
    fn shared_profile_with_domain_passes_prod() {
        let cfg: AppConfig = serde_yaml::from_str(&prod_multisite_yaml(
            r#"
sites:
  - name: app1
    port: 8081
    public_base_url: "https://app1.example.com"
    server_names: ["app1.example.com"]
    session_profile: isolated
  - name: app2
    port: 8082
    public_base_url: "https://app2.example.com"
    server_names: ["app2.example.com"]
    session_profile: isolated
session_profiles:
  isolated:
    cookie_name: "B1"
    cookie_domain: "example.com"
    allow_unmanaged_subdomains: true
"#,
        ))
        .unwrap();
        cfg.validate_with_env(true)
            .unwrap_or_else(|e| panic!("共享 profile 配 domain + ack 应通过，实际: {e}"));
    }

    // 规则 5：prod 下 cookie_domain 非空必须显式 allow_unmanaged_subdomains: true。

    #[test]
    fn prod_requires_allow_unmanaged_for_domain_cookie() {
        expect_prod_err(
            &prod_yaml(
                r#"
session:
  secure: true
  cookie_domain: "example.com"
"#,
            ),
            "allow_unmanaged_subdomains",
        );
    }

    #[test]
    fn prod_requires_ack_for_unreferenced_domain_profile() {
        expect_prod_err(
            &prod_yaml(
                r#"
session_profiles:
  isolated:
    cookie_name: "B1"
    cookie_domain: "example.com"
"#,
            ),
            "session_profiles[isolated].allow_unmanaged_subdomains",
        );
    }

    #[test]
    fn prod_accepts_acknowledged_domain_cookie() {
        let cfg: AppConfig = serde_yaml::from_str(&prod_yaml(
            r#"
session:
  secure: true
  cookie_domain: "example.com"
  allow_unmanaged_subdomains: true
"#,
        ))
        .unwrap();
        cfg.validate_with_env(true)
            .unwrap_or_else(|e| panic!("显式 ack 的 Domain cookie 应通过，实际: {e}"));
    }

    #[test]
    fn dev_warns_but_accepts_unacknowledged_domain_cookie() {
        expect_ok(
            r#"
session:
  cookie_domain: "example.com"
"#,
        );
    }
}

// ── 站点字段与 Host 白名单校验测试（§5.4 第 1/2/6/7/11 条 + §5.3 重复路由告警）──

#[cfg(test)]
mod site_validation_tests {
    use super::*;

    fn expect_err(yaml: &str, need: &str) {
        let cfg: AppConfig = serde_yaml::from_str(yaml).expect("YAML 可解析");
        let err = cfg
            .validate_with_env(false)
            .expect_err("dev 应校验失败")
            .to_string();
        assert!(err.contains(need), "错误应含 {need}，实际: {err}");
    }

    fn expect_ok(yaml: &str) {
        let cfg: AppConfig = serde_yaml::from_str(yaml).expect("YAML 可解析");
        cfg.validate_with_env(false)
            .unwrap_or_else(|e| panic!("dev 应校验通过，实际: {e}"));
    }

    fn expect_prod_err(yaml: &str, need: &str) {
        let cfg: AppConfig = serde_yaml::from_str(yaml).expect("YAML 可解析");
        let err = cfg
            .validate_with_env(true)
            .expect_err("prod 应校验失败")
            .to_string();
        assert!(err.contains(need), "错误应含 {need}，实际: {err}");
    }

    fn expect_prod_ok(yaml: &str) {
        let cfg: AppConfig = serde_yaml::from_str(yaml).expect("YAML 可解析");
        cfg.validate_with_env(true)
            .unwrap_or_else(|e| panic!("prod 应校验通过，实际: {e}"));
    }

    /// prod 下显式多站点最小配置。不含顶层 `server.public_base_url` / `trusted_hosts`
    /// （显式多站点下属 dead config，由站点级校验拒绝）。
    const PROD_BASE: &str = r#"
bff_secret:
  secret: "prod-real-secret"
  salt: "prod-real-salt-16b"
provider:
  session_store: redis
  cache: redis
  lock: redis
  redis_url: "redis://127.0.0.1:6379"
admin:
  auth_mode: token
  auth_token: "0123456789abcdef0123456789abcdef"
  enable_test_endpoints: false
session:
  secure: true
"#;

    fn prod_yaml(extra: &str) -> String {
        format!("{PROD_BASE}\n{extra}")
    }

    /// `server_names` 大小写与尾点归一，且与 public 主机去重后仅剩一个白名单条目。
    const MULTIDOT_YAML: &str = r#"
sites:
  - name: app1
    port: 8081
    server_names: ["App1.EXAMPLE.com."]
    public_base_url: "https://app1.example.com"
"#;

    // 规则 1：站点名非空、唯一、匹配 [a-z0-9-]+；保留名 admin 禁止。

    #[test]
    fn rejects_empty_site_name() {
        expect_err(
            r#"
sites:
  - name: ""
    port: 8081
"#,
            "sites[0].name",
        );
    }

    #[test]
    fn rejects_uppercase_site_name() {
        expect_err(
            r#"
sites:
  - name: "App1"
    port: 8081
"#,
            "sites[0].name",
        );
    }

    #[test]
    fn rejects_underscore_site_name() {
        expect_err(
            r#"
sites:
  - name: "bad_name"
    port: 8081
"#,
            "sites[0].name",
        );
    }

    #[test]
    fn rejects_reserved_admin_site_name() {
        expect_err(
            r#"
sites:
  - name: "admin"
    port: 8081
"#,
            "sites[0].name",
        );
    }

    #[test]
    fn rejects_duplicate_site_name() {
        expect_err(
            r#"
sites:
  - name: "app1"
    port: 8081
  - name: "app1"
    port: 8082
"#,
            "sites[1].name",
        );
    }

    #[test]
    fn accepts_valid_site_names_and_binds() {
        expect_ok(
            r#"
sites:
  - name: "default"
    port: 8081
    bind: "127.0.0.1"
  - name: "app-2"
    port: 8082
    bind: "::1"
"#,
        );
    }

    // 规则 2：端口非 0、唯一、≠ admin_port。

    #[test]
    fn rejects_zero_site_port() {
        expect_err(
            r#"
sites:
  - name: "app1"
    port: 0
"#,
            "sites[0].port",
        );
    }

    #[test]
    fn rejects_site_port_equal_to_admin_port() {
        expect_err(
            r#"
server:
  admin_port: 8443
sites:
  - name: "app1"
    port: 8443
"#,
            "sites[0].port",
        );
    }

    #[test]
    fn rejects_duplicate_site_port() {
        expect_err(
            r#"
sites:
  - name: "app1"
    port: 8081
  - name: "app2"
    port: 8081
"#,
            "sites[1].port",
        );
    }

    // 规则 3：bind 必须是可解析 IP。

    #[test]
    fn rejects_unparseable_site_bind() {
        expect_err(
            r#"
sites:
  - name: "app1"
    port: 8081
    bind: "not-an-ip"
"#,
            "sites[0].bind",
        );
    }

    // 规则 6（prod）：每站点 public_base_url 必填、与 server_names 一致、站点间唯一。

    #[test]
    fn prod_requires_site_public_base_url() {
        expect_prod_err(
            &prod_yaml(
                r#"
sites:
  - name: "app1"
    port: 8081
"#,
            ),
            "sites[0].public_base_url",
        );
    }

    #[test]
    fn prod_rejects_server_names_without_public_host() {
        expect_prod_err(
            &prod_yaml(
                r#"
sites:
  - name: "app1"
    port: 8081
    public_base_url: "https://app1.example.com"
    server_names: ["other.example.com"]
"#,
            ),
            "sites[0].server_names",
        );
    }

    #[test]
    fn prod_rejects_duplicate_public_base_url_across_sites() {
        expect_prod_err(
            &prod_yaml(
                r#"
sites:
  - name: "app1"
    port: 8081
    public_base_url: "https://app1.example.com"
  - name: "app2"
    port: 8082
    public_base_url: "https://app1.example.com"
"#,
            ),
            "sites[1].public_base_url",
        );
    }

    #[test]
    fn prod_accepts_unique_public_base_urls() {
        expect_prod_ok(&prod_yaml(
            r#"
sites:
  - name: "app1"
    port: 8081
    public_base_url: "https://app1.example.com"
    session_profile: app1
  - name: "app2"
    port: 8082
    public_base_url: "https://app2.example.com"
    session_profile: app2
session_profiles:
  app1: { cookie_name: "S1" }
  app2: { cookie_name: "S2" }
"#,
        ));
    }

    // 规则 6：显式多站点下顶层 server.public_base_url / trusted_hosts 为 dead config。

    #[test]
    fn rejects_server_public_base_url_with_explicit_sites() {
        expect_err(
            r#"
server:
  public_base_url: "https://bff.example.com"
sites:
  - name: "app1"
    port: 8081
"#,
            "server.public_base_url",
        );
    }

    #[test]
    fn rejects_server_trusted_hosts_with_explicit_sites() {
        expect_err(
            r#"
server:
  trusted_hosts: ["bff.example.com"]
sites:
  - name: "app1"
    port: 8081
"#,
            "server.trusted_hosts",
        );
    }

    #[test]
    fn accepts_non_default_business_port_with_explicit_sites() {
        expect_ok(
            r#"
server:
  business_port: 9090
sites:
  - name: "app1"
    port: 8081
"#,
        );
    }

    // 规则 7：server_names 仅主机名，站点内归一化后唯一。

    #[test]
    fn rejects_server_name_with_scheme() {
        expect_err(
            r#"
sites:
  - name: "app1"
    port: 8081
    server_names: ["https://x"]
"#,
            "sites[0].server_names",
        );
    }

    #[test]
    fn rejects_server_name_with_port() {
        expect_err(
            r#"
sites:
  - name: "app1"
    port: 8081
    server_names: ["x:8080"]
"#,
            "sites[0].server_names",
        );
    }

    #[test]
    fn rejects_wildcard_server_name() {
        expect_err(
            r#"
sites:
  - name: "app1"
    port: 8081
    server_names: ["*"]
"#,
            "sites[0].server_names",
        );
    }

    #[test]
    fn rejects_server_name_with_path() {
        expect_err(
            r#"
sites:
  - name: "app1"
    port: 8081
    server_names: ["a/b"]
"#,
            "sites[0].server_names",
        );
    }

    #[test]
    fn rejects_duplicate_server_names_after_normalization() {
        expect_err(
            r#"
sites:
  - name: "app1"
    port: 8081
    server_names: ["app1.example.com", "App1.EXAMPLE.com."]
"#,
            "sites[0].server_names",
        );
    }

    #[test]
    fn normalizes_server_names_case_and_trailing_dot() {
        let cfg: AppConfig = serde_yaml::from_str(MULTIDOT_YAML).unwrap();
        let s = &cfg.effective_sites()[0];
        assert_eq!(s.allowed_hosts, vec!["app1.example.com"]);
        cfg.validate_with_env(false)
            .unwrap_or_else(|e| panic!("归一化后的 server_names 应通过校验，实际: {e}"));
    }

    // 规则 11：prod 下多站点复用同一 spa.dir 仅告警（不 fail）。

    #[test]
    fn prod_accepts_shared_spa_dir_with_warning() {
        expect_prod_ok(&prod_yaml(
            r#"
sites:
  - name: "app1"
    port: 8081
    public_base_url: "https://app1.example.com"
    session_profile: app1
    spa: { dir: "shared/dist" }
  - name: "app2"
    port: 8082
    public_base_url: "https://app2.example.com"
    session_profile: app2
    spa: { dir: "shared/dist" }
session_profiles:
  app1: { cookie_name: "S1" }
  app2: { cookie_name: "S2" }
"#,
        ));
    }

    // 规则 8：legacy 冻结——无 sites 定义时 routes[].sites 注解导致路由不可命中，必须拒绝。

    #[test]
    fn rejects_route_sites_without_sites_definition() {
        expect_err(
            r#"
routes:
  - path: "/api/x"
    sites: ["app1"]
    type: static
    config: { status: 200 }
"#,
            "routes[0].sites",
        );
    }

    // 规则 3（§5.4）：session_profile 必须引用已存在的 profile。

    #[test]
    fn rejects_unknown_session_profile_reference() {
        expect_err(
            r#"
sites:
  - name: "app1"
    port: 8081
    session_profile: "missing"
"#,
            "sites[0].session_profile",
        );
    }

    #[test]
    fn accepts_existing_extra_session_profile_reference() {
        expect_ok(
            r#"
sites:
  - name: "app1"
    port: 8081
    session_profile: "isolated"
session_profiles:
  isolated: { cookie_name: "B1" }
"#,
        );
    }

    // §5.3：同站点可命中的完全同规格路由仅告警，不 fail。

    #[test]
    fn dev_accepts_duplicate_route_specs() {
        expect_ok(
            r#"
sites:
  - name: "app1"
    port: 8081
routes:
  - path: "/api/x"
    sites: ["app1"]
    methods: ["GET"]
    type: static
    config: { status: 200 }
  - path: "/api/x"
    sites: ["app1"]
    methods: ["GET"]
    type: static
    config: { status: 200 }
"#,
        );
    }

    #[test]
    fn legacy_accepts_duplicate_route_specs() {
        expect_ok(
            r#"
routes:
  - path: "/api/x"
    type: static
    config: { status: 200 }
  - path: "/api/x"
    type: static
    config: { status: 200 }
"#,
        );
    }
}
