//! 全局应用状态：配置快照（热重载）、Provider、OIDC 客户端缓存、指标等。
use crate::config::AppConfig;
use crate::middleware::circuit_breaker::CircuitBreakerRegistry;
use crate::oidc::OidcClientManager;
use crate::orchestration::step::StepContext;
use crate::orchestration::PipelineExecutor;
use crate::provider::{
    CacheProvider, InMemoryCache, InMemoryLock, LockProvider, RedisCache, RedisLock, RedisPool,
    RedisSessionStore,
};
use crate::scripting::ScriptEngine;
use arc_swap::ArcSwap;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tower_sessions::{MemoryStore, SessionStore};

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub provider: String,
    pub sub: String,
    pub created_at: i64,
    pub last_seen: i64,
}

#[derive(Clone)]
pub struct AppState {
    /// 配置快照：管理端导入时整体替换，读取零锁
    pub config: Arc<ArcSwap<AppConfig>>,
    /// 常规出网客户端（代理 http / 编排 / readiness；默认 30s 总超时）
    pub http: reqwest::Client,
    /// R1：流式出网客户端（SSE 等长连接：无总超时，仅 connect 超时 + TCP keepalive）
    pub http_stream: reqwest::Client,
    /// R13：OIDC 出网专用客户端（带超时、连接池复用）
    pub oidc_http: reqwest::Client,
    pub cache: Arc<dyn CacheProvider>,
    pub lock: Arc<dyn LockProvider>,
    /// 会话存储：按配置为 memory / redis（P0-1 多实例共享）
    pub session_store: Arc<dyn SessionStore>,
    /// Redis 连接池（仅当任一 provider 使用 redis 时存在）
    pub redis_pool: Option<RedisPool>,
    pub oidc_clients: Arc<OidcClientManager>,
    pub pipeline_executor: PipelineExecutor,
    pub sessions: Arc<RwLock<HashMap<String, SessionInfo>>>,
    pub breakers: CircuitBreakerRegistry,
    pub scripts: Arc<RwLock<HashMap<String, String>>>,
    pub prometheus: PrometheusHandle,
    /// R11：按上游的并发舱壁（0 = 不限制）
    pub upstream_limits: UpstreamLimits,
    /// P0-4：最近一次由本进程写入持久化文件的 sha256（避免 watcher 自触发）
    last_config_hash: Arc<std::sync::RwLock<Option<String>>>,
}

/// R11：按上游分组的并发信号量（舱壁），隔离慢上游对全局连接/任务的耗尽。
#[derive(Clone)]
pub struct UpstreamLimits {
    max_per_upstream: usize,
    semaphores: Arc<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>>,
}

/// R11：舱壁决策。
pub enum BulkheadDecision {
    /// 未启用（上限 0）
    Disabled,
    /// 已占用名额（持 permit 期间计数）
    Acquired(tokio::sync::OwnedSemaphorePermit),
    /// 已打满
    Saturated,
}

impl UpstreamLimits {
    pub fn new(max_per_upstream: usize) -> Self {
        Self {
            max_per_upstream,
            semaphores: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// 尝试占用一个名额（未启用 → Disabled；满 → Saturated）。
    pub fn try_acquire(&self, upstream: &str) -> BulkheadDecision {
        if self.max_per_upstream == 0 {
            return BulkheadDecision::Disabled;
        }
        let sem = {
            let mut map = self.semaphores.lock().expect("舱壁锁损坏");
            map.entry(upstream.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(self.max_per_upstream)))
                .clone()
        };
        match sem.try_acquire_owned() {
            Ok(permit) => BulkheadDecision::Acquired(permit),
            Err(_) => BulkheadDecision::Saturated,
        }
    }
}

impl AppState {
    pub fn new(config: AppConfig) -> anyhow::Result<Self> {
        config.validate()?;

        // P0-2：未固定对外地址时的部署提醒（生产防呆已在 validate 中强制）
        if config.server.public_base_url.is_none() {
            tracing::warn!(
                "未配置 server.public_base_url：OIDC 回调地址将按可信 Host 推导（仅限本机/白名单场景；生产请配置固定对外地址）"
            );
        }

        // 初始化加密密钥（必须在任何 crypto 操作之前）
        crate::utils::crypto::init(&config.bff_secret.secret, &config.bff_secret.salt)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        // P0-1：按配置构建 provider（memory | redis），Redis 连接惰性建立
        let need_redis = config.provider.session_store == "redis"
            || config.provider.cache == "redis"
            || config.provider.lock == "redis";
        let redis_pool = if need_redis {
            Some(RedisPool::new(&config.provider.redis_url)?)
        } else {
            None
        };
        let cache: Arc<dyn CacheProvider> = if config.provider.cache == "redis" {
            Arc::new(RedisCache::new(
                redis_pool
                    .clone()
                    .expect("cache=redis 时 redis_pool 必然存在"),
            ))
        } else {
            Arc::new(InMemoryCache::default())
        };
        let lock: Arc<dyn LockProvider> = if config.provider.lock == "redis" {
            Arc::new(RedisLock::new(
                redis_pool
                    .clone()
                    .expect("lock=redis 时 redis_pool 必然存在"),
            ))
        } else {
            Arc::new(InMemoryLock::new())
        };
        let session_store: Arc<dyn SessionStore> = if config.provider.session_store == "redis" {
            Arc::new(RedisSessionStore::new(
                redis_pool
                    .clone()
                    .expect("session_store=redis 时 redis_pool 必然存在"),
            ))
        } else {
            Arc::new(MemoryStore::default())
        };

        // 使用配置构建 HTTP 客户端（上游代理/编排/readiness 共用）
        let http = build_http_client(&config, config.http_client.timeout, true)?;
        // R1：SSE 等流式路径使用无总超时客户端（但仍受 connect 超时/TCP keepalive 保护），
        // 避免全局 30s 超时把长连接流拦腰截断。
        let http_stream = build_http_client(&config, None, true)?;
        // R13：OIDC 出网客户端——默认 15s 总超时（可被 http_client.timeout 显式覆盖），
        // 不跟随重定向（防 SSRF），避免 IdP 无响应时 /login、/auth/callback、refresh、
        // discovery 无限期挂起。
        let oidc_http = build_http_client(
            &config,
            Some(
                config
                    .http_client
                    .timeout
                    .unwrap_or(Duration::from_secs(15)),
            ),
            false,
        )?;

        let script_engine = ScriptEngine::new_with_max_duration(config.scripting.max_duration);
        let step_ctx = StepContext {
            http: http.clone(),
            cache: cache.clone(),
            scripts: script_engine,
            params: HashMap::new(),
            dry_run: false,
        };
        let cb_threshold = config.circuit_breaker.failure_threshold;
        let cb_window = config.circuit_breaker.failure_window;
        let cb_open_duration = config.circuit_breaker.open_duration;
        let upstream_limit = config.http_client.max_concurrent_per_upstream;

        Ok(Self {
            config: Arc::new(ArcSwap::from_pointee(config)),
            http,
            http_stream,
            oidc_http: oidc_http.clone(),
            cache,
            lock,
            session_store,
            redis_pool,
            oidc_clients: Arc::new(OidcClientManager::new(oidc_http)),
            pipeline_executor: PipelineExecutor::new(step_ctx),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            breakers: CircuitBreakerRegistry::new_with_config(
                cb_threshold,
                cb_window,
                cb_open_duration,
            ),
            scripts: Arc::new(RwLock::new(HashMap::new())),
            prometheus: init_metrics(),
            upstream_limits: UpstreamLimits::new(upstream_limit),
            last_config_hash: Arc::new(std::sync::RwLock::new(None)),
        })
    }

    /// 当前配置快照。
    pub fn cfg(&self) -> arc_swap::Guard<Arc<AppConfig>> {
        self.config.load()
    }

    /// 启动阶段依赖自检：Redis 启用时做一次 PING，fail-fast（避免运行期才发现不可达）。
    pub async fn verify_dependencies(&self) -> anyhow::Result<()> {
        if let Some(pool) = &self.redis_pool {
            pool.ping().await?;
        }
        // P0-4：持久化启用时确保目录可用（fail-fast，避免管理操作时才发现不可写）
        {
            let cfg = self.config.load();
            if cfg.persistence.enabled {
                let path = std::path::PathBuf::from(&cfg.persistence.path);
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(parent).map_err(|e| {
                            anyhow::anyhow!("创建持久化目录 {:?} 失败: {}", parent, e)
                        })?;
                    }
                }
                tracing::info!(path = %path.display(), "配置持久化已启用（管理端变更将落盘并支持多副本收敛）");
            }
        }
        Ok(())
    }

    /// R5：`sessions` 索引（管理端会话列表）单轮 GC。
    ///
    /// HashMap 中的条目在会话过期（store 中不存在）后必须清理，否则：
    /// - 内存随登录次数无界增长；
    /// - 管理端会话列表失真（显示已过期会话）。
    ///
    /// 返回清理掉的条目数。
    pub async fn gc_sessions_once(&self) -> usize {
        let ids: Vec<String> = self.sessions.read().await.keys().cloned().collect();
        let mut removed = 0usize;
        for id in ids {
            // tower-sessions 的 `Id` 为 32 字节 base64 字符串：非法 ID 直接移除
            let parsed = match id.parse::<tower_sessions::session::Id>() {
                Ok(p) => p,
                Err(_) => {
                    self.sessions.write().await.remove(&id);
                    removed += 1;
                    continue;
                }
            };
            match self.session_store.load(&parsed).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    self.sessions.write().await.remove(&id);
                    removed += 1;
                }
                // 存储后端临时故障：保守保留，下轮再试
                Err(_) => {}
            }
        }
        removed
    }

    /// R5：后台会话 GC 任务（由 main 在启动时 spawn，间隔由 `session.gc_interval` 控制）。
    pub async fn run_session_gc(self: Arc<Self>, interval: Duration) {
        if interval == Duration::ZERO {
            return;
        }
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let removed = self.gc_sessions_once().await;
            if removed > 0 {
                tracing::debug!(removed, "会话索引 GC：已清理过期会话条目");
            }
        }
    }
    /// P0-4：外部配置变更轮询（多副本共享存储 / 运维手工修改 runtime.yaml）。
    ///
    /// - 内容哈希与本进程最近写入一致 → 跳过（避免自我触发）；
    /// - 外部内容须通过校验且不试图变更 bff_secret，否则忽略并告警（不影响运行态）。
    pub async fn run_config_watcher(self: Arc<Self>) {
        loop {
            let (enabled, path, interval) = {
                let cfg = self.config.load();
                (
                    cfg.persistence.enabled,
                    cfg.persistence.path.clone(),
                    cfg.persistence.watch_interval,
                )
            };
            if !enabled || interval == Duration::ZERO {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            tokio::time::sleep(interval).await;

            let p = std::path::PathBuf::from(&path);
            let raw = match std::fs::read(&p) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let hash = sha256_hex(&raw);
            if self.is_own_config_write(&hash) {
                continue;
            }
            let mut cfg: AppConfig = match serde_yaml::from_slice(&raw) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(path = %p.display(), error = %e, "外部配置文件解析失败，忽略本次变更");
                    continue;
                }
            };
            let current = self.config.load();
            cfg.merge_sensitive_secrets(&current);
            drop(current);
            match self.apply_watched_config(cfg, hash).await {
                Ok(()) => {
                    tracing::info!(path = %p.display(), "检测到外部配置变更，已热重载");
                    let providers = { self.config.load().oidc.providers.clone() };
                    for provider in providers {
                        self.oidc_clients.invalidate(&provider.id).await;
                    }
                }
                Err(e) => {
                    tracing::warn!(path = %p.display(), error = %e, "外部配置校验失败，忽略本次变更");
                }
            }
        }
    }
    /// R5：更新会话索引的 `last_seen`（节流：距上次更新 <60s 时跳过，避免写放大）。
    pub async fn touch_session(&self, session_id: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut map = self.sessions.write().await;
        if let Some(info) = map.get_mut(session_id) {
            if now - info.last_seen >= 60 {
                info.last_seen = now;
            }
        }
    }

    /// 原子替换配置快照（热重载）。
    ///
    /// P0-4：持久化开启时先把脱敏配置原子落盘（失败则不应用，避免
    /// “内存改了、磁盘没改”的分裂）；重启与多副本据此收敛。
    pub fn replace_config(&self, cfg: AppConfig) -> anyhow::Result<()> {
        cfg.validate().map_err(|e| anyhow::anyhow!(e))?;
        // P0-3：bff_secret 不支持热更新。crypto::init 使用进程级 OnceLock，仅启动时生效；
        // 若允许替换，会造成「配置显示新密钥、加解密仍用旧密钥」的静默分裂，
        // 且新密钥反而会被导出接口泄露 → 显式拒绝并提示重启。
        {
            let current = self.config.load();
            if cfg.bff_secret.secret != current.bff_secret.secret
                || cfg.bff_secret.salt != current.bff_secret.salt
            {
                anyhow::bail!(
                    "bff_secret 不支持热更新（密钥派生在启动时完成）：拒绝本次变更，请修改配置后重启服务"
                );
            }
        }
        self.persist_config(&cfg)?;
        self.config.store(Arc::new(cfg));
        Ok(())
    }

    /// P0-4：把配置（脱敏）原子写入持久化文件；未启用时为 no-op。
    pub fn persist_config(&self, cfg: &AppConfig) -> anyhow::Result<()> {
        if !cfg.persistence.enabled {
            return Ok(());
        }
        let path = std::path::PathBuf::from(&cfg.persistence.path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| anyhow::anyhow!("创建持久化目录 {:?} 失败: {}", parent, e))?;
            }
        }
        let yaml = serde_yaml::to_string(&cfg.sanitized())
            .map_err(|e| anyhow::anyhow!("序列化持久化配置失败: {}", e))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, yaml.as_bytes())
            .map_err(|e| anyhow::anyhow!("写入持久化配置 {:?} 失败: {}", tmp, e))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| anyhow::anyhow!("提交持久化配置 {:?} 失败: {}", path, e))?;
        let digest = sha256_hex(yaml.as_bytes());
        let mut guard = self.last_config_hash.write().expect("哈希锁损坏");
        *guard = Some(digest);
        tracing::info!(path = %path.display(), "配置已持久化");
        Ok(())
    }

    /// P0-4：应用外部（另一副本/运维手工）写入的配置文件。
    ///
    /// 与 `replace_config` 的差异：不再回写文件（避免写放大），但同样校验与
    /// 拒绝 bff_secret 变更，并记录文件哈希避免自我触发。
    pub async fn apply_watched_config(
        &self,
        cfg: AppConfig,
        file_hash: String,
    ) -> anyhow::Result<()> {
        cfg.validate().map_err(|e| anyhow::anyhow!(e))?;
        {
            let current = self.config.load();
            if cfg.bff_secret.secret != current.bff_secret.secret
                || cfg.bff_secret.salt != current.bff_secret.salt
            {
                anyhow::bail!("外部配置试图变更 bff_secret（不支持热更新），已忽略");
            }
        }
        self.config.store(Arc::new(cfg));
        *self.last_config_hash.write().expect("哈希锁损坏") = Some(file_hash);
        Ok(())
    }

    /// P0-4：watcher 调用——文件内容是否为本进程自己写入的（是则跳过）。
    pub fn is_own_config_write(&self, file_hash: &str) -> bool {
        self.last_config_hash.read().expect("哈希锁损坏").as_deref() == Some(file_hash)
    }
}

/// 全局 Prometheus recorder 只安装一次（测试会构造多个 AppState）。
fn init_metrics() -> PrometheusHandle {
    static HANDLE: std::sync::OnceLock<PrometheusHandle> = std::sync::OnceLock::new();
    HANDLE
        .get_or_init(|| {
            PrometheusBuilder::new()
                .install_recorder()
                .expect("安装 Prometheus recorder 失败")
        })
        .clone()
}

/// sha256 十六进制（配置持久化哈希用）。
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// 构建 HTTP 客户端（含 mTLS / 自定义 CA）；`timeout` 为整体超时。
///
/// `follow_redirects=false` 时禁止跟随重定向（OIDC 出网防 SSRF）；
/// `true` 时保持 reqwest 默认策略（上游代理行为不变）。
fn build_http_client(
    cfg: &AppConfig,
    timeout: Option<Duration>,
    follow_redirects: bool,
) -> anyhow::Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .connect_timeout(cfg.http_client.connect_timeout)
        .pool_max_idle_per_host(cfg.http_client.pool_max_idle_per_host)
        .pool_idle_timeout(cfg.http_client.pool_idle_timeout);

    // R1/R16：TCP keepalive 探活（0 表示禁用）
    if cfg.http_client.tcp_keepalive > Duration::ZERO {
        b = b.tcp_keepalive(cfg.http_client.tcp_keepalive);
    }

    if let Some(t) = timeout {
        b = b.timeout(t);
    }
    if !follow_redirects {
        b = b.redirect(reqwest::redirect::Policy::none());
    }

    // 上游 mTLS 支持
    if let (Some(cert_path), Some(key_path)) = (
        &cfg.http_client.client_cert_path,
        &cfg.http_client.client_key_path,
    ) {
        let cert = std::fs::read(cert_path)?;
        let key = std::fs::read(key_path)?;
        let identity = reqwest::Identity::from_pem(&[cert, key].concat())?;
        b = b.identity(identity);
    }
    if let Some(ca_path) = &cfg.http_client.ca_cert_path {
        let ca = std::fs::read(ca_path)?;
        let cert = reqwest::Certificate::from_pem(&ca)?;
        b = b.add_root_certificate(cert);
    }

    Ok(b.build()?)
}

#[cfg(test)]
mod upstream_limits_tests {
    use super::{BulkheadDecision, UpstreamLimits};

    #[test]
    fn disabled_never_limits() {
        let limits = UpstreamLimits::new(0);
        for _ in 0..100 {
            assert!(matches!(
                limits.try_acquire("http://up"),
                BulkheadDecision::Disabled
            ));
        }
    }

    #[tokio::test]
    async fn saturates_at_configured_bound() {
        let limits = UpstreamLimits::new(2);
        let p1 = match limits.try_acquire("http://up") {
            BulkheadDecision::Acquired(p) => p,
            _ => panic!("应占用名额"),
        };
        let _p2 = match limits.try_acquire("http://up") {
            BulkheadDecision::Acquired(p) => p,
            _ => panic!("应占用名额"),
        };
        // 第 3 个请求被拒（舱壁打满）
        assert!(matches!(
            limits.try_acquire("http://up"),
            BulkheadDecision::Saturated
        ));
        // 不同上游互不影响
        assert!(matches!(
            limits.try_acquire("http://other"),
            BulkheadDecision::Acquired(_)
        ));
        // 释放后恢复
        drop(p1);
        assert!(matches!(
            limits.try_acquire("http://up"),
            BulkheadDecision::Acquired(_)
        ));
    }
}
