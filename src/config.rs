//! 配置定义与加载（figment 多层合并）。
//!
//! 合并优先级（低 → 高）：
//! 1. `config/base.yaml`
//! 2. `config/oidc/providers.yaml`
//! 3. `config/pipelines/*.yaml`（每个文件顶层 map 合并到 `pipelines` 键下）
//! 4. `config/routes/routes.yaml`
//! 5. 环境变量 `BFF_` 前缀（`__` 分隔层级）
//! 6. `config/env/{BFF_ENV}.yaml`

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
    #[serde(deserialize_with = "deserialize_env_or_default", default = "default_bff_secret")]
    pub secret: String,
    /// Argon2id 盐值，支持 `${BFF_SECRET_SALT:default}` 表达式
    #[serde(deserialize_with = "deserialize_env_or_default", default = "default_bff_salt")]
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
    /// 健康检查配置（就绪探针 / 存活探针）
    #[serde(default)]
    pub health: HealthConfig,
    #[serde(default)]
    pub pipelines: HashMap<String, PipelineDef>,
    #[serde(default)]
    pub routes: Vec<RouteDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_business_port")]
    pub business_port: u16,
    #[serde(default = "default_admin_port")]
    pub admin_port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            business_port: default_business_port(),
            admin_port: default_admin_port(),
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
    /// 全局请求超时（含连接+读取）
    #[serde(default, with = "humantime_serde::option")]
    pub timeout: Option<Duration>,
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
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: default_connect_timeout(),
            timeout: None,
            pool_max_idle_per_host: default_pool_max_idle(),
            pool_idle_timeout: default_pool_idle_timeout(),
            client_cert_path: None,
            client_key_path: None,
            ca_cert_path: None,
            retry_max_attempts: 0,
            retry_backoff: default_retry_backoff(),
        }
    }
}

fn default_retry_backoff() -> Duration {
    Duration::from_millis(100)
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
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            per_second: default_rate_per_second(),
            burst_size: default_rate_burst(),
        }
    }
}

fn default_rate_per_second() -> u64 {
    50
}
fn default_rate_burst() -> u32 {
    500
}

// ── CORS 配置 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorsConfig {
    /// 允许的来源列表（空 = 使用 permissive）
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// 是否允许所有来源（仅开发环境）
    #[serde(default)]
    pub permissive: bool,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowed_origins: vec![],
            permissive: false,
        }
    }
}

// ── 安全响应头配置 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityHeadersConfig {
    /// Content-Security-Policy
    #[serde(default = "default_csp")]
    pub content_security_policy: String,
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

// ── 请求体大小限制 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BodyLimitConfig {
    /// 请求体最大字节数
    #[serde(default = "default_body_limit")]
    pub max_bytes: usize,
}

impl Default for BodyLimitConfig {
    fn default() -> Self {
        Self {
            max_bytes: default_body_limit(),
        }
    }
}

fn default_body_limit() -> usize {
    10 * 1024 * 1024 // 10 MiB
}

// ── 熔断器配置 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    /// 连续失败阈值
    #[serde(default = "default_cb_failure_threshold")]
    pub failure_threshold: u32,
    /// 熔断打开持续时间
    #[serde(default = "default_cb_open_duration", with = "humantime_serde")]
    pub open_duration: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: default_cb_failure_threshold(),
            open_duration: default_cb_open_duration(),
        }
    }
}

fn default_cb_failure_threshold() -> u32 {
    5
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

// ── 健康检查配置 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthConfig {
    /// 就绪检查中需探测的上游列表（显式声明）
    /// 如果为空，则自动从 routes 中提取所有 proxy 类路由的 upstream 去重
    #[serde(default)]
    pub upstreams: Vec<String>,
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
            probe_timeout: default_probe_timeout(),
            allow_degraded: false,
            probe_path: default_probe_path(),
        }
    }
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
    #[serde(default)]
    pub secure: bool,
    #[serde(default = "default_true")]
    pub http_only: bool,
    #[serde(default = "default_same_site")]
    pub same_site: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            cookie_name: default_cookie_name(),
            secure: true, // P1-3: 默认安全
            http_only: true,
            same_site: default_same_site(),
        }
    }
}

fn default_cookie_name() -> String {
    "BFF_SESSION".into()
}
fn default_true() -> bool {
    true
}
fn default_same_site() -> String {
    "Strict".into()
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
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            ip_whitelist: default_ip_whitelist(),
            auth_mode: default_auth_mode(),
            auth_token: default_auth_token(),
            enable_test_endpoints: true,
            test_endpoint_rate_limit: default_test_rate_limit(),
        }
    }
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

/// 路由定义（统一模型）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteDef {
    /// 路由匹配路径前缀，如 "/api/users"、"/api/dashboard"
    pub path: String,

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
    /// 熔断阈值（连续失败次数），0 = 不熔断
    #[serde(default)]
    pub circuit_breaker_threshold: u32,
    /// 代理模式: "http" | "sse" | "websocket" | "auto"
    /// - http: 一次性请求-响应（默认）
    /// - sse: 流式透传 SSE
    /// - websocket: WebSocket 双向隧道
    /// - auto: 自动检测（根据 Upgrade/Content-Type）
    #[serde(default = "default_proxy_mode")]
    pub proxy_mode: String,

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

    /// 从 OIDC Session 提取，如 { "userId": "session.sub" }
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

        fig = fig
            .merge(Yaml::file(config_dir.join("routes/routes.yaml")))
            .merge(Env::prefixed("BFF_").split("__"));

        if let Ok(env) = std::env::var("BFF_ENV") {
            let env_file = config_dir.join("env").join(format!("{}.yaml", env));
            if env_file.is_file() {
                fig = fig.merge(Yaml::file(env_file));
            }
        }

        let cfg: AppConfig = fig.extract()?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
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

        // Provider 类型校验
        for kind in [
            &self.provider.session_store,
            &self.provider.cache,
            &self.provider.lock,
        ] {
            anyhow::ensure!(
                kind == "memory",
                "POC 阶段仅支持 memory provider，收到: {}",
                kind
            );
        }

        // 端口范围校验
        anyhow::ensure!(
            self.server.business_port > 0 && self.server.business_port != self.server.admin_port,
            "业务端口与管理端口不能相同"
        );

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

        // 限流参数校验
        anyhow::ensure!(
            self.rate_limit.per_second > 0,
            "rate_limit.per_second 必须 > 0"
        );

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

        Ok(())
    }

    /// 脱敏副本：隐藏 client_secret 与管理 token，用于导出。
    pub fn sanitized(&self) -> Self {
        let mut c = self.clone();
        for p in &mut c.oidc.providers {
            if !p.client_secret.is_empty() {
                p.client_secret = "***".into();
            }
        }
        if !c.admin.auth_token.is_empty() {
            c.admin.auth_token = "***".into();
        }
        c
    }
}
