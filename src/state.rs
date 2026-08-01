//! 全局应用状态：配置快照（热重载）、Provider、OIDC 客户端缓存、指标等。
use crate::config::AppConfig;
use crate::middleware::circuit_breaker::CircuitBreakerRegistry;
use crate::oidc::OidcClientManager;
use crate::orchestration::step::StepContext;
use crate::orchestration::PipelineExecutor;
use crate::provider::{CacheProvider, InMemoryCache, InMemoryLock, LockProvider};
use crate::scripting::ScriptEngine;
use arc_swap::ArcSwap;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_sessions::MemoryStore;

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
    pub cache: Arc<dyn CacheProvider>,
    pub lock: Arc<dyn LockProvider>,
    pub session_store: MemoryStore,
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

        // 初始化加密密钥（必须在任何 crypto 操作之前）
        crate::utils::crypto::init(&config.bff_secret.secret, &config.bff_secret.salt)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let cache: Arc<dyn CacheProvider> = Arc::new(InMemoryCache::default());
        let lock: Arc<dyn LockProvider> = Arc::new(InMemoryLock::new());

        // 使用配置构建 HTTP 客户端
        let mut http_builder = reqwest::Client::builder()
            .connect_timeout(config.http_client.connect_timeout)
            .pool_max_idle_per_host(config.http_client.pool_max_idle_per_host)
            .pool_idle_timeout(config.http_client.pool_idle_timeout);

        // 全局超时（覆盖 connect + read），若未配置则单独设置 read timeout
        if let Some(t) = config.http_client.timeout {
            http_builder = http_builder.timeout(t);
        }

        // 上游 mTLS 支持（P2-12）
        if let (Some(cert_path), Some(key_path)) = (
            &config.http_client.client_cert_path,
            &config.http_client.client_key_path,
        ) {
            let cert = std::fs::read(cert_path)?;
            let key = std::fs::read(key_path)?;
            let identity = reqwest::Identity::from_pem(&[cert, key].concat())?;
            http_builder = http_builder.identity(identity);
        }
        if let Some(ca_path) = &config.http_client.ca_cert_path {
            let ca = std::fs::read(ca_path)?;
            let cert = reqwest::Certificate::from_pem(&ca)?;
            http_builder = http_builder.add_root_certificate(cert);
        }

        let http = http_builder.build()?;

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
            cache,
            lock,
            session_store: MemoryStore::default(),
            oidc_clients: Arc::new(OidcClientManager::new()),
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

    /// 原子替换配置快照（热重载）。
    pub fn replace_config(&self, cfg: AppConfig) -> anyhow::Result<()> {
        cfg.validate().map_err(|e| anyhow::anyhow!(e))?;
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
