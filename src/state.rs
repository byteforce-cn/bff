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
    pub http: reqwest::Client,
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
        let cb_open_duration = config.circuit_breaker.open_duration;

        Ok(Self {
            config: Arc::new(ArcSwap::from_pointee(config)),
            http,
            oidc_http: oidc_http.clone(),
            cache,
            lock,
            session_store,
            redis_pool,
            oidc_clients: Arc::new(OidcClientManager::new(oidc_http)),
            pipeline_executor: PipelineExecutor::new(step_ctx),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            breakers: CircuitBreakerRegistry::new_with_config(cb_threshold, cb_open_duration),
            scripts: Arc::new(RwLock::new(HashMap::new())),
            prometheus: init_metrics(),
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
        Ok(())
    }

    /// 原子替换配置快照（热重载）。
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
        self.config.store(Arc::new(cfg));
        Ok(())
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
