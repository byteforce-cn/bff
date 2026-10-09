//! 结构指纹与差异（§5.6）。
//!
//! 热重载边界判定：`structural_diff(old, new)` 非空 → 需重启；
//! 否则按 `hot_applied_fields` 的粗粒度组名返回热生效清单。
//!
//! 实现约定（§5.6 实现注意）：
//! - 全部比较基于解析后值（`effective_sites()` / `resolved_session_profiles()`），
//!   与 §5.4 校验共用同一套归一化函数，避免“校验通过但指纹认为变了”；
//! - sites / profiles 以名字为键对齐、按 `BTreeMap`/`BTreeSet` 遍历，顺序稳定；
//! - 内层列表（hosts、origins、路径前缀、callback_path 集合）一律排序去重比较；
//! - `allow_unmanaged_subdomains` 属校验开关，不入指纹；
//! - `bff_secret.*` 不入指纹（保持现有拒绝语义，由配置替换流程处理）。
//!
//! 新增启动物化字段时必须同步更新 `requires_restart_fields`（集中构造指纹，禁止散落比较）。

use crate::config::{
    AdminConfig, AppConfig, HealthConfig, InputMapping, OidcProviderConfig, OutputMapping,
    PipelineDef, ResolvedSite, RouteDef, RouteTypeConfig, StepConfig, StepDef, TokenExchangeConfig,
    WebSocketTunnelConfig,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// 配置差异：`hot_applied` 为粗粒度热生效组名，`requires_restart` 为结构字段路径。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigDiff {
    pub hot_applied: Vec<String>,
    pub requires_restart: Vec<String>,
}

/// 收集 `impl IntoIterator` 为排序去重集合（指纹/组比较统一用集合语义，顺序不敏感）。
fn sorted_set<T: Ord>(items: impl IntoIterator<Item = T>) -> BTreeSet<T> {
    items.into_iter().collect()
}

/// 结构性差异：返回需重启的字段路径清单（顺序稳定，逐字段精确到叶子）。
pub fn requires_restart_fields(old: &AppConfig, new: &AppConfig) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();

    // server（admin 监听绑定；business_port 经 legacy 合成站点 port 覆盖）
    if old.server.admin_port != new.server.admin_port {
        fields.push("server.admin_port".into());
    }

    // provider（实例启动构建）
    if old.provider.session_store != new.provider.session_store {
        fields.push("provider.session_store".into());
    }
    if old.provider.cache != new.provider.cache {
        fields.push("provider.cache".into());
    }
    if old.provider.lock != new.provider.lock {
        fields.push("provider.lock".into());
    }
    if old.provider.redis_url != new.provider.redis_url {
        fields.push("provider.redis_url".into());
    }

    // http_client.*（共享 HTTP 客户端启动构建，全部字段）
    {
        let a = &old.http_client;
        let b = &new.http_client;
        if a.connect_timeout != b.connect_timeout {
            fields.push("http_client.connect_timeout".into());
        }
        if a.timeout != b.timeout {
            fields.push("http_client.timeout".into());
        }
        if a.tcp_keepalive != b.tcp_keepalive {
            fields.push("http_client.tcp_keepalive".into());
        }
        if a.pool_max_idle_per_host != b.pool_max_idle_per_host {
            fields.push("http_client.pool_max_idle_per_host".into());
        }
        if a.pool_idle_timeout != b.pool_idle_timeout {
            fields.push("http_client.pool_idle_timeout".into());
        }
        if a.client_cert_path != b.client_cert_path {
            fields.push("http_client.client_cert_path".into());
        }
        if a.client_key_path != b.client_key_path {
            fields.push("http_client.client_key_path".into());
        }
        if a.ca_cert_path != b.ca_cert_path {
            fields.push("http_client.ca_cert_path".into());
        }
        if a.retry_max_attempts != b.retry_max_attempts {
            fields.push("http_client.retry_max_attempts".into());
        }
        if a.retry_backoff != b.retry_backoff {
            fields.push("http_client.retry_backoff".into());
        }
        if a.max_concurrent_per_upstream != b.max_concurrent_per_upstream {
            fields.push("http_client.max_concurrent_per_upstream".into());
        }
    }

    // rate_limit（中间件层启动快照）
    {
        let a = &old.rate_limit;
        let b = &new.rate_limit;
        if a.per_second != b.per_second {
            fields.push("rate_limit.per_second".into());
        }
        if a.burst_size != b.burst_size {
            fields.push("rate_limit.burst_size".into());
        }
        if sorted_set(&a.skip_path_prefixes) != sorted_set(&b.skip_path_prefixes) {
            fields.push("rate_limit.skip_path_prefixes".into());
        }
    }

    // cors
    {
        let a = &old.cors;
        let b = &new.cors;
        if a.permissive != b.permissive {
            fields.push("cors.permissive".into());
        }
        if sorted_set(&a.allowed_origins) != sorted_set(&b.allowed_origins) {
            fields.push("cors.allowed_origins".into());
        }
    }

    // body_limit（max_response_bytes 属热路径快照，不在指纹内）
    if old.body_limit.max_bytes != new.body_limit.max_bytes {
        fields.push("body_limit.max_bytes".into());
    }

    // circuit_breaker.*（注册表启动构建）
    {
        let a = &old.circuit_breaker;
        let b = &new.circuit_breaker;
        if a.failure_threshold != b.failure_threshold {
            fields.push("circuit_breaker.failure_threshold".into());
        }
        if a.failure_window != b.failure_window {
            fields.push("circuit_breaker.failure_window".into());
        }
        if a.open_duration != b.open_duration {
            fields.push("circuit_breaker.open_duration".into());
        }
    }

    // scripting（引擎启动构建）
    if old.scripting.max_duration != new.scripting.max_duration {
        fields.push("scripting.max_duration".into());
    }

    // telemetry.*（启动初始化）
    {
        let a = &old.telemetry;
        let b = &new.telemetry;
        if a.otlp_endpoint != b.otlp_endpoint {
            fields.push("telemetry.otlp_endpoint".into());
        }
        if a.service_name != b.service_name {
            fields.push("telemetry.service_name".into());
        }
        if a.sample_ratio != b.sample_ratio {
            fields.push("telemetry.sample_ratio".into());
        }
    }

    // persistence（enabled 决定启动时是否落盘/轮询；path/watch_interval 为热字段）
    if old.persistence.enabled != new.persistence.enabled {
        fields.push("persistence.enabled".into());
    }

    // session（gc_interval 启动初始化；cookie 策略经 profiles 比较）
    if old.session.gc_interval != new.session.gc_interval {
        fields.push("session.gc_interval".into());
    }

    // session_profiles[<name>].*（解析后值，含 default 继承；
    // `allow_unmanaged_subdomains` 属校验开关，不入指纹）
    {
        let old_profiles = old.resolved_session_profiles();
        let new_profiles = new.resolved_session_profiles();
        let names: BTreeSet<&String> = old_profiles.keys().chain(new_profiles.keys()).collect();
        for name in names {
            match (old_profiles.get(name), new_profiles.get(name)) {
                (Some(a), Some(b)) => {
                    if a.cookie_name != b.cookie_name {
                        fields.push(format!("session_profiles[{name}].cookie_name"));
                    }
                    if a.cookie_domain != b.cookie_domain {
                        fields.push(format!("session_profiles[{name}].cookie_domain"));
                    }
                    if a.secure != b.secure {
                        fields.push(format!("session_profiles[{name}].secure"));
                    }
                    if a.http_only != b.http_only {
                        fields.push(format!("session_profiles[{name}].http_only"));
                    }
                    if a.same_site != b.same_site {
                        fields.push(format!("session_profiles[{name}].same_site"));
                    }
                    if a.ttl != b.ttl {
                        fields.push(format!("session_profiles[{name}].ttl"));
                    }
                }
                _ => fields.push(format!("session_profiles[{name}]")),
            }
        }
    }

    // sites[<name>].{port,bind,session_profile} 与站点增删（按名对齐；
    // name 变更 = 旧名删除 + 新名新增；legacy 合成 default 站点同样参与）
    {
        let old_eff = old.effective_sites();
        let new_eff = new.effective_sites();
        let old_sites: BTreeMap<&str, &ResolvedSite> =
            old_eff.iter().map(|s| (s.name.as_str(), s)).collect();
        let new_sites: BTreeMap<&str, &ResolvedSite> =
            new_eff.iter().map(|s| (s.name.as_str(), s)).collect();
        let names: BTreeSet<&str> = old_sites.keys().chain(new_sites.keys()).copied().collect();
        for name in names {
            match (old_sites.get(name), new_sites.get(name)) {
                (Some(a), Some(b)) => {
                    if a.port != b.port {
                        fields.push(format!("sites[{name}].port"));
                    }
                    if a.bind != b.bind {
                        fields.push(format!("sites[{name}].bind"));
                    }
                    if a.session_profile != b.session_profile {
                        fields.push(format!("sites[{name}].session_profile"));
                    }
                }
                _ => fields.push(format!("sites[{name}]")),
            }
        }
    }

    // oidc.providers.callback_paths（排序去重集合；集合内互换热生效，集合变化需重启）
    {
        let old_callbacks: BTreeSet<&str> = old
            .oidc
            .providers
            .iter()
            .map(|p| p.callback_path.as_str())
            .collect();
        let new_callbacks: BTreeSet<&str> = new
            .oidc
            .providers
            .iter()
            .map(|p| p.callback_path.as_str())
            .collect();
        if old_callbacks != new_callbacks {
            fields.push("oidc.providers.callback_paths".into());
        }
    }

    fields
}

/// 热生效变更：粗粒度组名清单（固定顺序）。
///
/// `old_scripts` / `new_scripts` 为运行期脚本注册表（`AppState.scripts`）快照——
/// 脚本不随配置快照变化（admin API 直写内存注册表），但同属 §5.6 热组 `scripts`。
pub fn hot_applied_fields(
    old: &AppConfig,
    new: &AppConfig,
    old_scripts: &HashMap<String, String>,
    new_scripts: &HashMap<String, String>,
) -> Vec<String> {
    let mut groups: Vec<String> = Vec::new();

    if !routes_equivalent(&old.routes, &new.routes) {
        groups.push("routes".into());
    }

    if !pipelines_equivalent(&old.pipelines, &new.pipelines) {
        groups.push("pipelines".into());
    }

    if old_scripts != new_scripts {
        groups.push("scripts".into());
    }

    // provider 内容（除 callback_path 集合外的字段；callback 集合变化归结构指纹）
    if !providers_content_equivalent(old, new) {
        groups.push("oidc.providers".into());
    }

    // 站点视图字段（增删站点属结构变更，不在此重复上报）
    if !site_views_equivalent(old, new) {
        groups.push("sites.view".into());
    }

    if old.persistence.path != new.persistence.path
        || old.persistence.watch_interval != new.persistence.watch_interval
    {
        groups.push("persistence".into());
    }

    if !health_equivalent(&old.health, &new.health) {
        groups.push("health".into());
    }

    if !websocket_equivalent(&old.websocket, &new.websocket) {
        groups.push("websocket".into());
    }

    if sorted_set(&old.token_refresh.skip_prefixes) != sorted_set(&new.token_refresh.skip_prefixes)
    {
        groups.push("token_refresh".into());
    }

    // admin（除 server.admin_port）
    if !admin_equivalent(&old.admin, &new.admin) {
        groups.push("admin".into());
    }

    // legacy 顶层 spa.dir（站点级 spa.dir 经 sites.view 的解析值覆盖）
    if old.spa.dir != new.spa.dir {
        groups.push("spa".into());
    }

    groups
}

/// 统一差异入口：§5.6 判定依据。
pub fn diff(
    old: &AppConfig,
    new: &AppConfig,
    old_scripts: &HashMap<String, String>,
    new_scripts: &HashMap<String, String>,
) -> ConfigDiff {
    ConfigDiff {
        hot_applied: hot_applied_fields(old, new, old_scripts, new_scripts),
        requires_restart: requires_restart_fields(old, new),
    }
}

// ── 热生效组的手写比较（新增字段时同步更新）──

/// provider 内容等价：按 id 对齐；`callback_path` 不参与（集合变化由结构指纹负责）；
/// 增删 provider 视为内容变化（callback 集合未变时纯热生效）。
fn providers_content_equivalent(old: &AppConfig, new: &AppConfig) -> bool {
    let old_map: BTreeMap<&str, &OidcProviderConfig> = old
        .oidc
        .providers
        .iter()
        .map(|p| (p.id.as_str(), p))
        .collect();
    let new_map: BTreeMap<&str, &OidcProviderConfig> = new
        .oidc
        .providers
        .iter()
        .map(|p| (p.id.as_str(), p))
        .collect();
    let ids: BTreeSet<&str> = old_map.keys().chain(new_map.keys()).copied().collect();
    ids.iter()
        .all(|id| match (old_map.get(id), new_map.get(id)) {
            (Some(a), Some(b)) => {
                a.display_name == b.display_name
                    && a.issuer_url == b.issuer_url
                    && a.client_id == b.client_id
                    && a.client_secret == b.client_secret
                    && a.scopes == b.scopes
                    && a.insecure_skip_id_token_verification
                        == b.insecure_skip_id_token_verification
                    && a.refresh_skew_secs == b.refresh_skew_secs
                    && a.shared_across_sites == b.shared_across_sites
            }
            _ => false,
        })
}

/// 站点视图等价（server_names / public_base_url / spa_dir / provider 绑定 /
/// security_headers / logout_scope，取解析后值）；增删站点不在此比较（结构指纹负责）。
fn site_views_equivalent(old: &AppConfig, new: &AppConfig) -> bool {
    let old_eff = old.effective_sites();
    let new_eff = new.effective_sites();
    let old_sites: BTreeMap<&str, &ResolvedSite> =
        old_eff.iter().map(|s| (s.name.as_str(), s)).collect();
    let new_sites: BTreeMap<&str, &ResolvedSite> =
        new_eff.iter().map(|s| (s.name.as_str(), s)).collect();
    let names: BTreeSet<&str> = old_sites.keys().chain(new_sites.keys()).copied().collect();
    names
        .iter()
        .all(|name| match (old_sites.get(name), new_sites.get(name)) {
            (Some(a), Some(b)) => {
                sorted_set(&a.server_names) == sorted_set(&b.server_names)
                    && a.public_base_url == b.public_base_url
                    && a.spa_dir == b.spa_dir
                    && a.default_provider == b.default_provider
                    && sorted_set(&a.allowed_providers) == sorted_set(&b.allowed_providers)
                    && a.security_headers == b.security_headers
                    && a.logout_scope == b.logout_scope
            }
            _ => true, // 增删站点：结构指纹负责
        })
}

fn health_equivalent(a: &HealthConfig, b: &HealthConfig) -> bool {
    sorted_set(&a.upstreams) == sorted_set(&b.upstreams)
        && a.cache_ttl == b.cache_ttl
        && a.probe_timeout == b.probe_timeout
        && a.allow_degraded == b.allow_degraded
        && a.probe_path == b.probe_path
}

fn websocket_equivalent(a: &WebSocketTunnelConfig, b: &WebSocketTunnelConfig) -> bool {
    a.connect_timeout == b.connect_timeout
        && a.idle_timeout == b.idle_timeout
        && a.heartbeat_interval == b.heartbeat_interval
        && a.max_message_bytes == b.max_message_bytes
}

fn admin_equivalent(a: &AdminConfig, b: &AdminConfig) -> bool {
    sorted_set(&a.ip_whitelist) == sorted_set(&b.ip_whitelist)
        && a.auth_mode == b.auth_mode
        && a.auth_token == b.auth_token
        && a.enable_test_endpoints == b.enable_test_endpoints
        && a.test_endpoint_rate_limit == b.test_endpoint_rate_limit
        && a.max_body_bytes == b.max_body_bytes
        && a.auth_fail_limit_per_minute == b.auth_fail_limit_per_minute
        && a.trusted_proxies == b.trusted_proxies
}

/// pipelines 注册表等价：按名字对齐，逐定义比较（复用 `pipeline_equivalent`）。
fn pipelines_equivalent(
    a: &HashMap<String, PipelineDef>,
    b: &HashMap<String, PipelineDef>,
) -> bool {
    a.len() == b.len()
        && a.iter().all(|(name, def)| {
            b.get(name)
                .map(|other| pipeline_equivalent(Some(def), Some(other)))
                .unwrap_or(false)
        })
}

/// 路由等价：逐字段手写比较（顺序敏感——最长前缀优先依赖顺序）。
fn routes_equivalent(a: &[RouteDef], b: &[RouteDef]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| route_equivalent(x, y))
}

fn route_equivalent(a: &RouteDef, b: &RouteDef) -> bool {
    a.path == b.path
        && a.sites == b.sites
        && a.methods == b.methods
        && a.description == b.description
        && a.auth_required == b.auth_required
        && a.route_type == b.route_type
        && route_config_equivalent(&a.config, &b.config)
        && input_mapping_equivalent(&a.input_mapping, &b.input_mapping)
        && output_mapping_equivalent(&a.output_mapping, &b.output_mapping)
}

fn route_config_equivalent(a: &RouteTypeConfig, b: &RouteTypeConfig) -> bool {
    a.upstream == b.upstream
        && a.strip_prefix == b.strip_prefix
        && a.circuit_breaker_threshold == b.circuit_breaker_threshold
        && a.proxy_mode == b.proxy_mode
        && a.timeout == b.timeout
        && a.forward_set_cookie == b.forward_set_cookie
        && token_exchange_equivalent(a.token_exchange.as_ref(), b.token_exchange.as_ref())
        && a.pipeline == b.pipeline
        && pipeline_equivalent(a.pipeline_inline.as_ref(), b.pipeline_inline.as_ref())
        && a.script == b.script
        && a.script_inline == b.script_inline
        && a.status == b.status
        && a.body == b.body
        && a.headers == b.headers
}

fn token_exchange_equivalent(
    a: Option<&TokenExchangeConfig>,
    b: Option<&TokenExchangeConfig>,
) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.token_endpoint == b.token_endpoint
                && a.client_id == b.client_id
                && a.client_secret == b.client_secret
                && a.client_auth_method == b.client_auth_method
                && a.subject_token_type == b.subject_token_type
                && a.audience == b.audience
                && a.scope == b.scope
                && a.requested_token_type == b.requested_token_type
                && a.actor_token_type == b.actor_token_type
                && a.cache_ttl == b.cache_ttl
        }
        _ => false,
    }
}

fn pipeline_equivalent(a: Option<&PipelineDef>, b: Option<&PipelineDef>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.strategy.timeout == b.strategy.timeout
                && a.strategy.error_handling == b.strategy.error_handling
                && a.steps.len() == b.steps.len()
                && a.steps
                    .iter()
                    .zip(b.steps.iter())
                    .all(|(x, y)| step_equivalent(x, y))
        }
        _ => false,
    }
}

fn step_equivalent(a: &StepDef, b: &StepDef) -> bool {
    a.id == b.id
        && a.step_type == b.step_type
        && a.depends_on == b.depends_on
        && step_config_equivalent(&a.config, &b.config)
}

fn step_config_equivalent(a: &StepConfig, b: &StepConfig) -> bool {
    a.url == b.url
        && a.method == b.method
        && a.timeout == b.timeout
        && a.cache_ttl == b.cache_ttl
        && a.headers == b.headers
        && a.body == b.body
        && a.script == b.script
}

fn input_mapping_equivalent(a: &InputMapping, b: &InputMapping) -> bool {
    a.from_query == b.from_query
        && a.from_body == b.from_body
        && a.from_path == b.from_path
        && a.from_header == b.from_header
        && a.from_session == b.from_session
        && a.from_env == b.from_env
        && a.defaults == b.defaults
}

fn output_mapping_equivalent(a: &OutputMapping, b: &OutputMapping) -> bool {
    a.wrap == b.wrap && a.status_map == b.status_map && a.rename == b.rename && a.pick == b.pick
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, LogoutScope, RouteDef, SiteConfig};
    use std::time::Duration;

    /// §5.1 / 附录 B 两站点样例（另含一个 `iso` profile 与两个不同 callback_path 的 provider，
    /// 保证 `oidc.providers[0]` / `[1]` 索引与互换测试有效）。
    const MULTISITE_YAML: &str = r#"
server:
  admin_port: 8443
session:
  cookie_name: "BFF_SESSION_V2"
  cookie_domain: ".example.com"
  secure: true
  http_only: true
  same_site: "Lax"
  ttl: "336h"
  allow_unmanaged_subdomains: true
session_profiles:
  iso:
    cookie_name: "BFF_ISO"
    cookie_domain: ""
    http_only: true
    secure: true
    same_site: "Strict"
    ttl: "24h"
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
    bind: "0.0.0.0"
    server_names: ["app2.example.com"]
    public_base_url: "https://app2.example.com"
    session_profile: default
    spa: { dir: "apps/app2/dist" }
    oidc:
      default_provider: app2
      allowed_providers: [app2]
    logout_scope: global
oidc:
  providers:
    - id: app1
      display_name: "App1"
      issuer_url: "https://idp.example.com"
      client_id: "app1-client"
      client_secret: "app1-secret"
      callback_path: "/auth/callback-a"
      shared_across_sites: false
    - id: app2
      display_name: "App2"
      issuer_url: "https://idp.example.com"
      client_id: "app2-client"
      client_secret: "app2-secret"
      callback_path: "/auth/callback-b"
      shared_across_sites: false
"#;

    fn sample_multisite() -> AppConfig {
        serde_yaml::from_str(MULTISITE_YAML).expect("测试样例解析失败")
    }

    /// 表驱动用例：`(期望字段路径/组名, 变更闭包)`。
    type MutationCase = (&'static str, Box<dyn FnOnce(&mut AppConfig)>);

    fn changed(mutate: impl FnOnce(&mut AppConfig)) -> (AppConfig, AppConfig) {
        let old = sample_multisite();
        let mut new = old.clone();
        mutate(&mut new);
        (old, new)
    }

    /// 测试用差异入口：脚本注册表不参与（配置级用例）。
    fn diff_cfg(old: &AppConfig, new: &AppConfig) -> ConfigDiff {
        let scripts = HashMap::new();
        diff(old, new, &scripts, &scripts)
    }

    fn static_route(path: &str) -> RouteDef {
        serde_yaml::from_str(&format!(
            "path: \"{path}\"\ntype: static\nconfig:\n  status: 200\n"
        ))
        .unwrap()
    }

    /// §5.6 结构指纹的每个字段都必须被检出（表驱动，路径精确匹配）。
    #[test]
    fn each_structural_field_is_detected() {
        let cases: Vec<MutationCase> = vec![
            (
                "server.admin_port",
                Box::new(|c| c.server.admin_port = 9443),
            ),
            // provider
            (
                "provider.session_store",
                Box::new(|c| c.provider.session_store = "redis".into()),
            ),
            (
                "provider.cache",
                Box::new(|c| c.provider.cache = "redis".into()),
            ),
            (
                "provider.lock",
                Box::new(|c| c.provider.lock = "redis".into()),
            ),
            (
                "provider.redis_url",
                Box::new(|c| c.provider.redis_url = "redis://127.0.0.1:6379".into()),
            ),
            // http_client.*（全部字段）
            (
                "http_client.connect_timeout",
                Box::new(|c| c.http_client.connect_timeout = Duration::from_secs(10)),
            ),
            (
                "http_client.timeout",
                Box::new(|c| c.http_client.timeout = Some(Duration::from_secs(15))),
            ),
            (
                "http_client.tcp_keepalive",
                Box::new(|c| c.http_client.tcp_keepalive = Duration::from_secs(120)),
            ),
            (
                "http_client.pool_max_idle_per_host",
                Box::new(|c| c.http_client.pool_max_idle_per_host = 16),
            ),
            (
                "http_client.pool_idle_timeout",
                Box::new(|c| c.http_client.pool_idle_timeout = Duration::from_secs(45)),
            ),
            (
                "http_client.client_cert_path",
                Box::new(|c| c.http_client.client_cert_path = Some("/tmp/cert.pem".into())),
            ),
            (
                "http_client.client_key_path",
                Box::new(|c| c.http_client.client_key_path = Some("/tmp/key.pem".into())),
            ),
            (
                "http_client.ca_cert_path",
                Box::new(|c| c.http_client.ca_cert_path = Some("/tmp/ca.pem".into())),
            ),
            (
                "http_client.retry_max_attempts",
                Box::new(|c| c.http_client.retry_max_attempts = 3),
            ),
            (
                "http_client.retry_backoff",
                Box::new(|c| c.http_client.retry_backoff = Duration::from_millis(200)),
            ),
            (
                "http_client.max_concurrent_per_upstream",
                Box::new(|c| c.http_client.max_concurrent_per_upstream = 8),
            ),
            // rate_limit
            (
                "rate_limit.per_second",
                Box::new(|c| c.rate_limit.per_second = 100),
            ),
            (
                "rate_limit.burst_size",
                Box::new(|c| c.rate_limit.burst_size = 1000),
            ),
            (
                "rate_limit.skip_path_prefixes",
                Box::new(|c| c.rate_limit.skip_path_prefixes = vec!["/assets".into()]),
            ),
            // cors
            ("cors.permissive", Box::new(|c| c.cors.permissive = true)),
            (
                "cors.allowed_origins",
                Box::new(|c| c.cors.allowed_origins = vec!["https://app1.example.com".into()]),
            ),
            // body_limit
            (
                "body_limit.max_bytes",
                Box::new(|c| c.body_limit.max_bytes = 5 * 1024 * 1024),
            ),
            // circuit_breaker
            (
                "circuit_breaker.failure_threshold",
                Box::new(|c| c.circuit_breaker.failure_threshold = 10),
            ),
            (
                "circuit_breaker.failure_window",
                Box::new(|c| c.circuit_breaker.failure_window = Duration::from_secs(30)),
            ),
            (
                "circuit_breaker.open_duration",
                Box::new(|c| c.circuit_breaker.open_duration = Duration::from_secs(15)),
            ),
            // scripting
            (
                "scripting.max_duration",
                Box::new(|c| c.scripting.max_duration = Duration::from_secs(4)),
            ),
            // telemetry
            (
                "telemetry.otlp_endpoint",
                Box::new(|c| c.telemetry.otlp_endpoint = Some("http://otel:4317".into())),
            ),
            (
                "telemetry.service_name",
                Box::new(|c| c.telemetry.service_name = "bff-2".into()),
            ),
            (
                "telemetry.sample_ratio",
                Box::new(|c| c.telemetry.sample_ratio = 0.25),
            ),
            // persistence
            (
                "persistence.enabled",
                Box::new(|c| c.persistence.enabled = true),
            ),
            // session
            (
                "session.gc_interval",
                Box::new(|c| c.session.gc_interval = Duration::from_secs(60)),
            ),
            // session_profiles（解析后值，含 default；default 归属顶层 session）
            (
                "session_profiles[default].cookie_name",
                Box::new(|c| c.session.cookie_name = "X".into()),
            ),
            (
                "session_profiles[default].cookie_domain",
                Box::new(|c| c.session.cookie_domain = Some(".other.example.com".into())),
            ),
            (
                "session_profiles[default].secure",
                Box::new(|c| c.session.secure = false),
            ),
            (
                "session_profiles[default].http_only",
                Box::new(|c| c.session.http_only = false),
            ),
            (
                "session_profiles[default].same_site",
                Box::new(|c| c.session.same_site = "Strict".into()),
            ),
            (
                "session_profiles[default].ttl",
                Box::new(|c| c.session.ttl = Some(Duration::from_secs(3600))),
            ),
            (
                "session_profiles[iso].cookie_name",
                Box::new(|c| {
                    c.session_profiles.get_mut("iso").unwrap().cookie_name = Some("ISO-X".into())
                }),
            ),
            (
                "session_profiles[iso].cookie_domain",
                Box::new(|c| {
                    c.session_profiles.get_mut("iso").unwrap().cookie_domain =
                        Some(".iso.example.com".into())
                }),
            ),
            (
                "session_profiles[iso].secure",
                Box::new(|c| c.session_profiles.get_mut("iso").unwrap().secure = Some(false)),
            ),
            (
                "session_profiles[iso].http_only",
                Box::new(|c| c.session_profiles.get_mut("iso").unwrap().http_only = Some(false)),
            ),
            (
                "session_profiles[iso].same_site",
                Box::new(|c| {
                    c.session_profiles.get_mut("iso").unwrap().same_site = Some("Lax".into())
                }),
            ),
            (
                "session_profiles[iso].ttl",
                Box::new(|c| {
                    c.session_profiles.get_mut("iso").unwrap().ttl = Some(Duration::from_secs(7200))
                }),
            ),
            // profile 增删（新增即使无站点引用也改变指纹）
            (
                "session_profiles[extra]",
                Box::new(|c| {
                    c.session_profiles
                        .insert("extra".into(), Default::default());
                }),
            ),
            (
                "session_profiles[iso]",
                Box::new(|c| {
                    c.session_profiles.remove("iso");
                }),
            ),
            // sites
            ("sites[app1].port", Box::new(|c| c.sites[0].port = 9081)),
            (
                "sites[app1].bind",
                Box::new(|c| c.sites[0].bind = "127.0.0.1".into()),
            ),
            (
                "sites[app1].session_profile",
                Box::new(|c| c.sites[0].session_profile = "iso".into()),
            ),
            (
                "sites[app3]",
                Box::new(|c| {
                    c.sites.push(SiteConfig {
                        name: "app3".into(),
                        port: 8083,
                        bind: "0.0.0.0".into(),
                        server_names: vec![],
                        public_base_url: None,
                        session_profile: "default".into(),
                        spa: None,
                        oidc: Default::default(),
                        logout_scope: LogoutScope::Global,
                        security_headers: None,
                    })
                }),
            ),
            (
                "sites[app1]",
                Box::new(|c| {
                    c.sites.remove(0);
                }),
            ),
            // oidc
            (
                "oidc.providers.callback_paths",
                Box::new(|c| c.oidc.providers[0].callback_path = "/cb".into()),
            ),
        ];
        for (field, m) in cases {
            let (old, new) = changed(m);
            let fields = requires_restart_fields(&old, &new);
            assert!(
                fields.iter().any(|f| f == field),
                "{field} 未检出: {fields:?}"
            );
        }
    }

    /// 指纹顺序不敏感（sites / providers 重排无差异）；新增 profile 改变指纹（即使无站点引用）。
    #[test]
    fn order_insensitive_and_resolved_profiles() {
        let mut a = sample_multisite();
        let mut b = sample_multisite();
        b.sites.reverse();
        b.oidc.providers.reverse();
        assert!(requires_restart_fields(&a, &b).is_empty());
        a.session_profiles.insert("x".into(), Default::default());
        // 新增 profile 改变指纹（即使无站点引用）
        assert!(!requires_restart_fields(&a, &b).is_empty());
    }

    /// callback_path 集合内互换不改变启动注册的路径集合 → 热生效（§5.6）。
    #[test]
    fn callback_path_swap_between_providers_is_hot() {
        let (old, mut new) = changed(|_| {});
        let p0 = new.oidc.providers[0].callback_path.clone();
        let p1 = new.oidc.providers[1].callback_path.clone();
        new.oidc.providers[0].callback_path = p1;
        new.oidc.providers[1].callback_path = p0;
        assert!(requires_restart_fields(&old, &new).is_empty());
    }

    /// 热字段（routes / provider 内容）不要求重启，但出现在 hot_applied 清单。
    #[test]
    fn hot_fields_not_structural() {
        let (old, mut new) = changed(|c| c.routes.push(static_route("/api/x")));
        new.oidc.providers[0].client_id = "new".into();
        let fields = requires_restart_fields(&old, &new);
        assert!(fields.is_empty(), "热字段不应要求重启: {fields:?}");
        let hot = hot_applied_fields(&old, &new, &HashMap::new(), &HashMap::new());
        assert!(hot.contains(&"routes".to_string()));
        assert!(hot.contains(&"oidc.providers".to_string()));
    }

    /// 每个热生效组名都必须能被检出（且不触发结构差异）。
    #[test]
    fn each_hot_group_is_reported() {
        let cases: Vec<MutationCase> = vec![
            (
                "routes",
                Box::new(|c| c.routes.push(static_route("/api/hot"))),
            ),
            (
                "pipelines",
                Box::new(|c| {
                    c.pipelines
                        .insert("p-hot".into(), serde_yaml::from_str("steps: []\n").unwrap());
                }),
            ),
            (
                "oidc.providers",
                Box::new(|c| c.oidc.providers[0].display_name = "Renamed".into()),
            ),
            (
                "sites.view",
                Box::new(|c| c.sites[0].server_names = vec!["app1b.example.com".into()]),
            ),
            (
                "persistence",
                Box::new(|c| c.persistence.path = "config/state/other.yaml".into()),
            ),
            ("health", Box::new(|c| c.health.probe_path = "/live".into())),
            (
                "websocket",
                Box::new(|c| c.websocket.heartbeat_interval = Duration::from_secs(15)),
            ),
            (
                "token_refresh",
                Box::new(|c| c.token_refresh.skip_prefixes.push("/custom".into())),
            ),
            ("admin", Box::new(|c| c.admin.auth_token = "rotated".into())),
            ("spa", Box::new(|c| c.spa.dir = "other/dist".into())),
        ];
        for (group, m) in cases {
            let (old, new) = changed(m);
            let restart = requires_restart_fields(&old, &new);
            assert!(restart.is_empty(), "{group} 变更不应要求重启: {restart:?}");
            let hot = hot_applied_fields(&old, &new, &HashMap::new(), &HashMap::new());
            assert!(hot.contains(&group.to_string()), "{group} 未检出: {hot:?}");
        }
    }

    /// `diff` 同时返回两个清单。
    #[test]
    fn diff_combines_hot_and_structural() {
        let (old, mut new) = changed(|c| c.server.admin_port = 9443);
        new.oidc.providers[0].client_id = "rotated".into();
        let d = diff_cfg(&old, &new);
        assert!(d
            .requires_restart
            .contains(&"server.admin_port".to_string()));
        assert!(d.hot_applied.contains(&"oidc.providers".to_string()));
    }

    /// 归一化后等价的值（尾斜杠 / 大小写 / 前导点）不得产生差异（指纹取解析后值，§5.6）；
    /// `allow_unmanaged_subdomains` 属校验开关，不入指纹。
    #[test]
    fn resolved_normalization_prevents_spurious_diffs() {
        let old = sample_multisite();
        let mut new = old.clone();
        new.sites[0].public_base_url = Some("https://app1.example.com/".into());
        new.session.cookie_domain = Some("Example.com".into());
        new.sites[0].server_names = vec!["APP1.EXAMPLE.COM.".into()];
        new.session.allow_unmanaged_subdomains = false;
        assert!(requires_restart_fields(&old, &new).is_empty());
        assert!(hot_applied_fields(&old, &new, &HashMap::new(), &HashMap::new()).is_empty());
    }

    /// legacy（无 sites）→ 显式多站点：站点增删属结构变更（§5.6）。
    #[test]
    fn legacy_to_explicit_multisite_is_structural() {
        let old: AppConfig = serde_yaml::from_str(
            r#"
oidc:
  providers:
    - id: p1
      issuer_url: "https://idp.example.com"
      client_id: "cid"
"#,
        )
        .unwrap();
        let new = sample_multisite();
        let fields = requires_restart_fields(&old, &new);
        assert!(fields.iter().any(|f| f == "sites[default]"), "{fields:?}");
        assert!(fields.iter().any(|f| f == "sites[app1]"), "{fields:?}");
        assert!(fields.iter().any(|f| f == "sites[app2]"), "{fields:?}");
    }

    /// §5.6 热表：pipelines 变更属热生效——报 `pipelines` 组、不要求重启。
    #[test]
    fn pipelines_only_change_is_hot() {
        let (old, mut new) = changed(|_| {});
        new.pipelines.insert(
            "p1".into(),
            serde_yaml::from_str(
                r#"
strategy: {}
steps:
  - id: s
    type: script
    config: { script: "1" }
"#,
            )
            .unwrap(),
        );
        let d = diff_cfg(&old, &new);
        assert!(d.requires_restart.is_empty(), "{:?}", d.requires_restart);
        assert!(
            d.hot_applied.iter().any(|g| g == "pipelines"),
            "{:?}",
            d.hot_applied
        );
    }

    /// §5.6 热表：scripts 注册表变化报 `scripts` 组、不要求重启（注册表为运行期状态，
    /// 不随配置快照变化，经差异入口显式传入）。
    #[test]
    fn scripts_only_change_is_hot() {
        let (old, new) = changed(|_| {});
        let old_scripts: HashMap<String, String> = HashMap::new();
        let mut new_scripts: HashMap<String, String> = HashMap::new();
        new_scripts.insert("s.js".into(), "1".into());
        let d = diff(&old, &new, &old_scripts, &new_scripts);
        assert!(d.requires_restart.is_empty(), "{:?}", d.requires_restart);
        assert!(
            d.hot_applied.iter().any(|g| g == "scripts"),
            "{:?}",
            d.hot_applied
        );
    }
}
