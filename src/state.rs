//! 全局应用状态：配置快照（热重载）、Provider、OIDC 客户端缓存、指标等。
use crate::config::AppConfig;
use crate::config_fingerprint::{diff, ConfigDiff};
use crate::middleware::circuit_breaker::CircuitBreakerRegistry;
use crate::oidc::OidcClientManager;
use crate::orchestration::step::StepContext;
use crate::orchestration::PipelineExecutor;
use crate::provider::session::{build_layer, DynSessionStore};
use crate::provider::{
    CacheProvider, InMemoryCache, InMemoryLock, LockProvider, RedisCache, RedisLock, RedisPool,
    RedisSessionStore,
};
use crate::scripting::ScriptEngine;
use crate::site::{PrebuiltSecurityHeaders, SiteHandle, SiteView};
use arc_swap::ArcSwap;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tower_sessions::SessionManagerLayer;
use tower_sessions::{MemoryStore, SessionStore};

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub provider: String,
    pub sub: String,
    pub created_at: i64,
    pub last_seen: i64,
}

/// 配置应用失败：`Rejected`（校验/构建/密钥拒绝，旧配置保持）或
/// `RequiresRestart`（结构指纹非空，不替换快照、不落盘、不动视图，§5.6）。
#[derive(Debug)]
pub enum ConfigApplyError {
    Rejected(anyhow::Error),
    RequiresRestart(ConfigDiff),
}

impl std::fmt::Display for ConfigApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigApplyError::Rejected(e) => write!(f, "{e}"),
            ConfigApplyError::RequiresRestart(d) => write!(
                f,
                "变更涉及需重启的结构配置: {}",
                d.requires_restart.join(", ")
            ),
        }
    }
}

impl std::error::Error for ConfigApplyError {}

#[derive(Clone)]
pub struct AppState {
    /// 配置快照：管理端导入时整体替换，读取零锁
    pub config: Arc<ArcSwap<AppConfig>>,
    /// 常规出网客户端（代理 http / 编排 / readiness；默认 30s 总超时）
    pub http: reqwest::Client,
    /// 流式出网客户端（SSE 等长连接：无总超时，仅 connect 超时 + TCP keepalive）
    pub http_stream: reqwest::Client,
    /// OIDC 出网专用客户端（带超时、连接池复用）
    pub oidc_http: reqwest::Client,
    pub cache: Arc<dyn CacheProvider>,
    pub lock: Arc<dyn LockProvider>,
    /// 会话存储：按配置为 memory / redis（多实例共享）
    pub session_store: Arc<dyn SessionStore>,
    /// Redis 连接池（仅当任一 provider 使用 redis 时存在）
    pub redis_pool: Option<RedisPool>,
    pub oidc_clients: Arc<OidcClientManager>,
    pub pipeline_executor: PipelineExecutor,
    pub sessions: Arc<RwLock<HashMap<String, SessionInfo>>>,
    pub breakers: CircuitBreakerRegistry,
    pub scripts: Arc<RwLock<HashMap<String, String>>>,
    pub prometheus: PrometheusHandle,
    /// 按上游的并发舱壁（0 = 不限制）
    pub upstream_limits: UpstreamLimits,
    /// 会话层映射：profile 名 → 启动构建的 `SessionManagerLayer`（§5.2 每 profile 一个）
    pub session_layers: HashMap<String, SessionManagerLayer<DynSessionStore>>,
    /// 站点视图映射（配置替换时预构建；`ArcSwap` 不 Clone，必须套 `Arc`）
    site_views: Arc<ArcSwap<HashMap<String, Arc<SiteView>>>>,
    /// 最近一次由本进程写入持久化文件的 sha256（避免 watcher 自触发）
    last_config_hash: Arc<std::sync::RwLock<Option<String>>>,
    /// 管理写接口的读-改-写串行化锁：读快照 → 变更 → 应用必须原子完成，
    /// 否则两个并发请求基于同一旧快照各自存储，后者静默覆盖前者（200 但行为不变）。
    apply_lock: Arc<tokio::sync::Mutex<()>>,
}

/// 按上游分组的并发信号量（舱壁），隔离慢上游对全局连接/任务的耗尽。
#[derive(Clone)]
pub struct UpstreamLimits {
    max_per_upstream: usize,
    semaphores: Arc<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>>,
}

/// 舱壁决策。
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

        // 未固定对外地址时的部署提醒（生产防呆已在 validate 中强制）
        if config.server.public_base_url.is_none() {
            tracing::warn!(
                "未配置 server.public_base_url：OIDC 回调地址将按可信 Host 推导（仅限本机/白名单场景；生产请配置固定对外地址）"
            );
        }

        // 初始化加密密钥（必须在任何 crypto 操作之前）
        crate::utils::crypto::init(&config.bff_secret.secret, &config.bff_secret.salt)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        // 按配置构建 provider（memory | redis），Redis 连接惰性建立
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
        // SSE 等流式路径使用无总超时客户端（但仍受 connect 超时/TCP keepalive 保护），
        // 避免全局 30s 超时把长连接流拦腰截断。
        let http_stream = build_http_client(&config, None, true)?;
        // OIDC 出网客户端——默认 15s 总超时（可被 http_client.timeout 显式覆盖），
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

        // §5.2：每 profile 构建一个 SessionManagerLayer（cookie 策略取解析后值）
        let session_layers = build_session_layers(session_store.clone(), &config)?;
        // §9：站点视图 + 安全响应头在配置替换时预构建（请求路径零解析成本）
        let site_views = Arc::new(ArcSwap::from_pointee(build_site_views(&config)?));

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
            session_layers,
            site_views,
            last_config_hash: Arc::new(std::sync::RwLock::new(None)),
            apply_lock: Arc::new(Mutex::new(())),
        })
    }

    /// 当前配置快照。
    pub fn cfg(&self) -> arc_swap::Guard<Arc<AppConfig>> {
        self.config.load()
    }

    /// 按站点名取预构建视图（每请求调用；未命中返回 None）。
    pub fn site_view(&self, name: &str) -> Option<Arc<SiteView>> {
        self.site_views.load().get(name).cloned()
    }

    /// §6.1：按 effective sites 构建站点句柄（启动期一次性；layer 从 map 取）。
    pub fn site_handles(&self) -> anyhow::Result<Vec<Arc<SiteHandle>>> {
        let cfg = self.config.load();
        let sites = cfg.effective_sites();
        sites
            .iter()
            .map(|site| {
                let session_layer = self
                    .session_layers
                    .get(&site.session_profile)
                    .cloned()
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "站点 [{}] 引用的 session profile [{}] 无对应会话层",
                            site.name,
                            site.session_profile
                        )
                    })?;
                Ok(Arc::new(SiteHandle {
                    name: site.name.clone(),
                    port: site.port,
                    bind: site.bind.clone(),
                    session_profile: site.session_profile.clone(),
                    session_layer,
                    legacy: site.legacy,
                }))
            })
            .collect()
    }

    /// 管理端导入：整体替换配置快照（热重载，§5.6）。
    ///
    /// 结构指纹非空 → `RequiresRestart`（不替换快照、不落盘、不动视图）；
    /// 否则预构建站点视图 → 落盘（启用时）→ 原子替换 → provider 缓存集中失效。
    pub async fn apply_config(&self, cfg: AppConfig) -> Result<ConfigDiff, ConfigApplyError> {
        let _guard = self.apply_lock.lock().await;
        self.apply_inner(cfg, true).await
    }

    /// 管理端细粒度写接口（provider / pipeline / routes 增删改）共用的
    /// 读-改-写原子应用：在 `apply_lock` 内读取当前快照 → 执行变更闭包 →
    /// 走完整 `apply_inner` 管线（校验 → diff → 预构建 → 落盘 → 存储 → 失效）。
    ///
    /// 闭包必须基于传入的最新快照做变更（不要在闭包外再读 `self.cfg()`），
    /// 否则仍会基于过期快照决策，重新引入丢更新竞态。
    pub async fn apply_config_mut(
        &self,
        mutate: impl FnOnce(&mut AppConfig) + Send,
    ) -> Result<ConfigDiff, ConfigApplyError> {
        let _guard = self.apply_lock.lock().await;
        let mut cfg = self.config.load().as_ref().clone();
        mutate(&mut cfg);
        self.apply_inner(cfg, true).await
    }

    /// 启动阶段依赖自检：Redis 启用时做一次 PING，fail-fast（避免运行期才发现不可达）。
    pub async fn verify_dependencies(&self) -> anyhow::Result<()> {
        if let Some(pool) = &self.redis_pool {
            pool.ping().await?;
        }
        // 持久化启用时确保目录可用（fail-fast，避免管理操作时才发现不可写）
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
                // 可写性探针：目录属主/挂载权限错误时在启动阶段就暴露
                let probe = path.with_extension("probe");
                match std::fs::write(&probe, b"ok") {
                    Ok(()) => {
                        let _ = std::fs::remove_file(&probe);
                    }
                    Err(e) => {
                        anyhow::bail!(
                            "持久化路径不可写（{}: {}）：请检查卷挂载属主/权限（容器以数值 UID 10001 运行；K8s 可配 fsGroup）",
                            probe.display(),
                            e
                        );
                    }
                }
                tracing::info!(path = %path.display(), "配置持久化已启用（管理端变更将落盘并支持多副本收敛）");
            }
        }
        Ok(())
    }

    /// `sessions` 索引（管理端会话列表）单轮 GC。
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

    /// 后台会话 GC 任务（由 main 在启动时 spawn，间隔由 `session.gc_interval` 控制）。
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
    /// 外部配置变更轮询（多副本共享存储 / 运维手工修改 runtime.yaml）。
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
                }
                Err(e) => {
                    tracing::warn!(path = %p.display(), error = %e, "外部配置校验失败，忽略本次变更");
                }
            }
        }
    }
    /// 更新会话索引的 `last_seen`（节流：距上次更新 <60s 时跳过，避免写放大）。
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

    /// 配置替换内部实现（import 与 watcher 共用，§5.6 顺序）：
    /// 校验 → bff_secret 检查 → 结构指纹 → 预构建视图 → 落盘（可选）→ 替换快照
    /// → 视图存储 → provider 缓存集中失效（old∪new 全部 id）。
    ///
    /// 任何一步失败都不改变旧配置/旧视图（§5.6：不产生中间态）。
    async fn apply_inner(
        &self,
        cfg: AppConfig,
        persist: bool,
    ) -> Result<ConfigDiff, ConfigApplyError> {
        cfg.validate().map_err(ConfigApplyError::Rejected)?;
        // 脚本注册表为运行期状态，不随配置快照变化；快照仅用于差异入口的 `scripts` 组。
        let scripts_snapshot = self.scripts.read().await.clone();
        let (d, new_views, provider_ids) = {
            let current = self.config.load();
            // bff_secret 不支持热更新。crypto::init 使用进程级 OnceLock，仅启动时生效；
            // 若允许替换，会造成「配置显示新密钥、加解密仍用旧密钥」的静默分裂，
            // 且新密钥反而会被导出接口泄露 → 显式拒绝并提示重启。
            if cfg.bff_secret.secret != current.bff_secret.secret
                || cfg.bff_secret.salt != current.bff_secret.salt
            {
                return Err(ConfigApplyError::Rejected(anyhow::anyhow!(
                    "bff_secret 不支持热更新（密钥派生在启动时完成）：拒绝本次变更，请修改配置后重启服务"
                )));
            }
            let d = diff(&current, &cfg, &scripts_snapshot, &scripts_snapshot);
            if !d.requires_restart.is_empty() {
                return Err(ConfigApplyError::RequiresRestart(d));
            }
            // 预构建新站点视图（失败 → Rejected，旧配置/旧视图不变）
            let new_views = build_site_views(&cfg).map_err(ConfigApplyError::Rejected)?;
            // old∪new 的全部 provider id（去重，集中失效钩子的依据）
            let mut ids: Vec<String> = current
                .oidc
                .providers
                .iter()
                .map(|p| p.id.clone())
                .collect();
            ids.extend(cfg.oidc.providers.iter().map(|p| p.id.clone()));
            ids.sort();
            ids.dedup();
            (d, new_views, ids)
        };
        if persist {
            self.persist_config(&cfg)
                .map_err(ConfigApplyError::Rejected)?;
        }
        self.config.store(Arc::new(cfg));
        self.site_views.store(Arc::new(new_views));
        // §5.6 集中失效钩子：provider 内容热更新后必须使 OidcClientManager 缓存失效，
        // 不依赖各 handler 记得调用（避免“配置改了但行为不变”的隐性故障）
        for id in provider_ids {
            self.oidc_clients.invalidate(&id).await;
        }
        Ok(d)
    }

    /// 应用外部（另一副本/运维手工）写入的配置文件。
    ///
    /// 与 `apply_config` 的差异：不回写文件（避免写放大），但同样校验、拒绝
    /// bff_secret 变更、重建视图并完成 provider 缓存失效；结构差异 → warn + Err（保持旧配置）。
    pub async fn apply_watched_config(
        &self,
        cfg: AppConfig,
        file_hash: String,
    ) -> anyhow::Result<()> {
        let _guard = self.apply_lock.lock().await;
        match self.apply_inner(cfg, false).await {
            Ok(_) => {}
            Err(ConfigApplyError::RequiresRestart(d)) => {
                tracing::warn!(
                    requires_restart = ?d.requires_restart,
                    "外部配置含需重启的结构差异，忽略本次变更"
                );
                anyhow::bail!(
                    "外部配置含需重启的结构差异（{}），已忽略",
                    d.requires_restart.join(", ")
                );
            }
            Err(ConfigApplyError::Rejected(e)) => return Err(e),
        }
        // 不回写文件，但记录文件哈希避免自我触发
        *self.last_config_hash.write().expect("哈希锁损坏") = Some(file_hash);
        Ok(())
    }

    /// 把配置（脱敏）原子写入持久化文件；未启用时为 no-op。
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

    /// watcher 调用——文件内容是否为本进程自己写入的（是则跳过）。
    pub fn is_own_config_write(&self, file_hash: &str) -> bool {
        self.last_config_hash.read().expect("哈希锁损坏").as_deref() == Some(file_hash)
    }
}

/// §5.2：按解析后 profile 构建会话层映射（启动与配置替换共用）。
pub(crate) fn build_session_layers(
    session_store: Arc<dyn SessionStore>,
    cfg: &AppConfig,
) -> anyhow::Result<HashMap<String, SessionManagerLayer<DynSessionStore>>> {
    let mut layers = HashMap::new();
    for (name, profile) in cfg.resolved_session_profiles() {
        layers.insert(name, build_layer(session_store.clone(), &profile)?);
    }
    Ok(layers)
}

/// §9：预构建站点视图映射（安全响应头在此解析，请求路径零解析成本）。
pub(crate) fn build_site_views(cfg: &AppConfig) -> anyhow::Result<HashMap<String, Arc<SiteView>>> {
    let mut views = HashMap::new();
    for site in cfg.effective_sites() {
        let headers = Arc::new(PrebuiltSecurityHeaders::build(
            &cfg.security_headers,
            site.security_headers.as_ref(),
        )?);
        views.insert(
            site.name.clone(),
            Arc::new(SiteView::from_resolved(&site, headers)),
        );
    }
    Ok(views)
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
        .pool_idle_timeout(cfg.http_client.pool_idle_timeout)
        // 出网统一 HTTP/1.1：连接池与超时语义简单可预期（reqwest 未启用 `http2` feature，
        // h2 仅出现在 tonic/OTLP 独立栈）。openidconnect 4.0 迁移后此处不再与供应链例外相关。
        .http1_only();

    // TCP keepalive 探活（0 表示禁用）
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

#[cfg(test)]
mod apply_config_tests {
    use super::*;
    use crate::config::{AppConfig, PipelineDef, RouteDef, SpaConfig};

    /// 本地双站点配置（Task 10 前测试不可用 common::multisite_config；dev 语义：
    /// 无 public_base_url，loopback 兜底；fake issuer URL 不发起真实网络请求）。
    fn multisite_cfg() -> AppConfig {
        serde_yaml::from_str(
            r#"
server:
  admin_port: 8443
session:
  cookie_domain: ".test"
  allow_unmanaged_subdomains: true
sites:
  - name: app1
    port: 8081
    bind: "127.0.0.1"
    server_names: ["app1.test"]
    session_profile: default
    oidc:
      default_provider: idp1
  - name: app2
    port: 8083
    bind: "127.0.0.1"
    server_names: ["app2.test"]
    session_profile: default
    oidc:
      default_provider: idp2
oidc:
  providers:
    - id: idp1
      issuer_url: "http://idp1.test"
      client_id: "app1-client"
      callback_path: "/auth/callback-app1"
    - id: idp2
      issuer_url: "http://idp2.test"
      client_id: "app2-client"
      callback_path: "/auth/callback-app2"
"#,
        )
        .expect("测试配置解析失败")
    }

    /// 启动期即构建会话层映射与站点视图（§5.2 / §9）。
    #[tokio::test]
    async fn site_handles_and_views_built_at_startup() {
        let state = AppState::new(multisite_cfg()).unwrap();
        let handles = state.site_handles().unwrap();
        assert_eq!(handles.len(), 2, "{handles:?}");
        assert_eq!(handles[0].name, "app1");
        assert_eq!(handles[0].session_profile, "default");
        assert_eq!(handles[0].port, 8081);
        assert_eq!(handles[1].name, "app2");
        assert_eq!(handles[1].port, 8083);
        assert!(state.site_view("app1").is_some());
        assert_eq!(state.site_view("app2").unwrap().spa_dir, "frontend/dist");
        assert!(state.site_view("nope").is_none());
        assert!(state.session_layers.contains_key("default"));
    }

    /// §5.6：结构变更（port）→ RequiresRestart，旧配置/旧视图保持不变。
    #[tokio::test]
    async fn apply_config_rejects_structural_change() {
        let state = AppState::new(multisite_cfg()).unwrap();
        let mut next = state.cfg().as_ref().clone();
        next.sites[0].port += 1;
        match state.apply_config(next).await {
            Err(ConfigApplyError::RequiresRestart(d)) => {
                assert!(d.requires_restart.iter().any(|f| f == "sites[app1].port"));
            }
            other => panic!("应 RequiresRestart，实际 {other:?}"),
        }
        assert_eq!(state.cfg().sites[0].port, 8081, "旧配置必须保持");
    }

    /// §5.6：删除站点属结构变更，必须要求重启。
    #[tokio::test]
    async fn apply_config_rejects_site_removal() {
        let state = AppState::new(multisite_cfg()).unwrap();
        let mut next = state.cfg().as_ref().clone();
        next.sites.remove(1);
        let err = state.apply_config(next).await.unwrap_err();
        assert!(
            matches!(err, ConfigApplyError::RequiresRestart(_)),
            "删站点必须要求重启: {err:?}"
        );
        assert_eq!(state.cfg().sites.len(), 2, "旧配置必须保持");
    }

    /// 热变更（routes / spa.dir）→ Ok(diff)，视图按新配置重建。
    #[tokio::test]
    async fn apply_config_applies_hot_change_and_rebuilds_views() {
        let state = AppState::new(multisite_cfg()).unwrap();
        let mut next = state.cfg().as_ref().clone();
        let route: RouteDef = serde_yaml::from_str(
            r#"
path: "/api/hot"
methods: ["GET"]
type: static
config: { status: 200 }
"#,
        )
        .unwrap();
        next.routes.push(route);
        next.sites[0].spa = Some(SpaConfig {
            dir: "apps/app1-new/dist".into(),
        });
        let d = state.apply_config(next).await.unwrap();
        assert!(d.requires_restart.is_empty(), "{:?}", d.requires_restart);
        assert!(
            d.hot_applied.iter().any(|g| g == "routes"),
            "{:?}",
            d.hot_applied
        );
        assert!(
            d.hot_applied.iter().any(|g| g == "sites.view"),
            "{:?}",
            d.hot_applied
        );
        let view = state.site_view("app1").expect("app1 视图应存在");
        assert_eq!(view.spa_dir, "apps/app1-new/dist", "视图应按新配置重建");
        assert_eq!(state.cfg().routes.len(), 1);
        assert!(state.site_view("app2").is_some(), "app2 视图仍应存在");
    }

    fn pipeline_def(step_id: &str) -> PipelineDef {
        serde_yaml::from_str(&format!(
            r#"
strategy:
  timeout: 10s
steps:
  - id: {step_id}
    type: script
    config:
      script: "return {{}};"
"#
        ))
        .expect("测试 pipeline 解析失败")
    }

    /// 并发读-改-写串行化：两个并发 `apply_config_mut` 各插入不同 pipeline，
    /// 两者都必须保留。主任务先持有 `scripts` 写锁——`apply_inner` 的第一步
    /// 就是读 `scripts`，两个写任务因此都会在「已各自克隆旧快照、尚未存储」
    /// 的窗口上被挡住；主任务统一放行后，未串行化时两个 store 竞速必丢其一
    /// （后写的覆盖先写的，200 但行为不变），串行化后第二个任务在锁内读到
    /// 第一个任务的结果，两者都保留。
    #[tokio::test]
    async fn apply_config_mut_serializes_concurrent_writes() {
        let state = AppState::new(multisite_cfg()).unwrap();
        let scripts_guard = state.scripts.write().await;
        let s1 = state.clone();
        let s2 = state.clone();
        let t1 = tokio::spawn(async move {
            s1.apply_config_mut(|cfg| {
                cfg.pipelines.insert("p1".into(), pipeline_def("s1"));
            })
            .await
        });
        let t2 = tokio::spawn(async move {
            s2.apply_config_mut(|cfg| {
                cfg.pipelines.insert("p2".into(), pipeline_def("s2"));
            })
            .await
        });
        // 给两个任务足够时间推进到 `scripts` 读锁处（各自已持旧快照），再统一放行。
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(scripts_guard);
        let (r1, r2) = tokio::join!(t1, t2);
        r1.unwrap().unwrap();
        r2.unwrap().unwrap();
        let cfg = state.cfg();
        let keys: Vec<&String> = cfg.pipelines.keys().collect();
        assert!(
            cfg.pipelines.contains_key("p1"),
            "并发写丢失 p1，现存: {keys:?}"
        );
        assert!(
            cfg.pipelines.contains_key("p2"),
            "并发写丢失 p2，现存: {keys:?}"
        );
    }
}
