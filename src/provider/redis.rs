//! Redis provider：Cache / Lock / Session 的分布式实现（P0-1：多实例共享状态）。
//!
//! - **连接**：`redis::aio::ConnectionManager`（断线自动重连 + Clone 多路复用）。
//!   经 [`RedisPool`] **惰性建立**：`AppState::new` 保持同步，连接在首次使用时创建；
//!   服务启动阶段调用 `AppState::verify_dependencies()` 做 PING fail-fast。
//! - **Cache**：`GET` / `SET ... PX（条目级 TTL）` / `DEL`。
//! - **Lock**：`SET key token NX PX hold_ttl` 获取；释放用 Lua 校验持有者，避免误删他者锁。
//! - **Session**：`Record` JSON 序列化，key = `bff:sess:{id}`，TTL 由记录过期时刻推导。

use crate::provider::lock::LockGuard;
use crate::provider::{CacheProvider, LockProvider};
use async_trait::async_trait;
use redis::aio::ConnectionManager;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::OnceCell;
use tower_sessions::session::{Id, Record};
use tower_sessions::session_store::{self, SessionStore};

/// 惰性共享 Redis 连接池。
#[derive(Clone, Debug)]
pub struct RedisPool {
    client: redis::Client,
    conn: Arc<OnceCell<ConnectionManager>>,
}

impl RedisPool {
    pub fn new(redis_url: &str) -> anyhow::Result<Self> {
        let client = redis::Client::open(redis_url)
            .map_err(|e| anyhow::anyhow!("provider.redis_url 非法: {}", e))?;
        Ok(Self {
            client,
            conn: Arc::new(OnceCell::new()),
        })
    }

    /// 获取（必要时建立）共享连接。
    pub async fn conn(&self) -> redis::RedisResult<ConnectionManager> {
        let c = self
            .conn
            .get_or_try_init(|| async { self.client.get_connection_manager().await })
            .await?;
        Ok(c.clone())
    }

    /// 健康检查：PING（启动阶段 fail-fast 用）。
    pub async fn ping(&self) -> anyhow::Result<()> {
        let mut conn = self
            .conn()
            .await
            .map_err(|e| anyhow::anyhow!("Redis 连接失败: {}", e))?;
        let _: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .map_err(|e| anyhow::anyhow!("Redis PING 失败: {}", e))?;
        Ok(())
    }
}

// ── Cache ──

/// Redis 缓存实现（供限流桶 / 编排缓存 / 令牌交换缓存跨实例共享）。
#[derive(Clone, Debug)]
pub struct RedisCache {
    pool: RedisPool,
}

impl RedisCache {
    pub fn new(pool: RedisPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl CacheProvider for RedisCache {
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        let mut conn = match self.pool.conn().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "Redis 连接不可用，缓存读取按未命中处理");
                return None;
            }
        };
        match redis::cmd("GET")
            .arg(key)
            .query_async::<Option<Vec<u8>>>(&mut conn)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(%key, error = %e, "RedisCache::get 失败（按未命中处理）");
                None
            }
        }
    }

    async fn set(&self, key: &str, value: Vec<u8>, ttl: Duration) {
        let ms = ttl.as_millis().max(1).min(u64::MAX as u128) as u64;
        let mut conn = match self.pool.conn().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "Redis 连接不可用，缓存写入丢弃");
                return;
            }
        };
        let res: redis::RedisResult<()> = redis::cmd("SET")
            .arg(key)
            .arg(value)
            .arg("PX")
            .arg(ms)
            .query_async(&mut conn)
            .await;
        if let Err(e) = res {
            tracing::warn!(%key, error = %e, "RedisCache::set 失败");
        }
    }

    async fn delete(&self, key: &str) {
        let mut conn = match self.pool.conn().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "Redis 连接不可用，缓存删除丢弃");
                return;
            }
        };
        let res: redis::RedisResult<i64> = redis::cmd("DEL").arg(key).query_async(&mut conn).await;
        if let Err(e) = res {
            tracing::warn!(%key, error = %e, "RedisCache::delete 失败");
        }
    }
}

// ── Lock ──

/// Redis 分布式锁（SET NX PX + Lua 安全释放）。
#[derive(Clone, Debug)]
pub struct RedisLock {
    pool: RedisPool,
}

impl RedisLock {
    pub fn new(pool: RedisPool) -> Self {
        Self { pool }
    }
}

/// 获取锁的重试间隔。
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// 释放锁：仅当 value == token 时删除（防止删掉他者持有的锁）。
const RELEASE_SCRIPT: &str = r#"
if redis.call('GET', KEYS[1]) == ARGV[1] then
    return redis.call('DEL', KEYS[1])
end
return 0
"#;

#[async_trait]
impl LockProvider for RedisLock {
    async fn acquire(
        &self,
        key: &str,
        wait_timeout: Duration,
        hold_ttl: Duration,
    ) -> Option<Box<dyn LockGuard>> {
        let token = uuid::Uuid::new_v4().to_string();
        let ms = hold_ttl.as_millis().max(1).min(u64::MAX as u128) as u64;
        let deadline = tokio::time::Instant::now() + wait_timeout;

        loop {
            let mut conn = match self.pool.conn().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(%key, error = %e, "Redis 连接不可用，锁获取失败");
                    return None;
                }
            };
            let acquired: redis::RedisResult<Option<String>> = redis::cmd("SET")
                .arg(key)
                .arg(&token)
                .arg("PX")
                .arg(ms)
                .arg("NX")
                .query_async(&mut conn)
                .await;
            match acquired {
                Ok(Some(_)) => {
                    return Some(Box::new(RedisLockGuard {
                        pool: self.pool.clone(),
                        key: key.to_string(),
                        token,
                    }));
                }
                Ok(None) => {} // 锁被占用 → 等待重试
                Err(e) => {
                    tracing::warn!(%key, error = %e, "RedisLock::acquire 失败");
                    return None;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(LOCK_RETRY_INTERVAL).await;
        }
    }
}

struct RedisLockGuard {
    pool: RedisPool,
    key: String,
    token: String,
}

#[async_trait]
impl LockGuard for RedisLockGuard {
    async fn release(self: Box<Self>) {
        let mut conn = match self.pool.conn().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(key = %self.key, error = %e, "锁释放失败（将由 TTL 自动过期）");
                return;
            }
        };
        let res: redis::RedisResult<i64> = redis::Script::new(RELEASE_SCRIPT)
            .key(&self.key)
            .arg(&self.token)
            .invoke_async(&mut conn)
            .await;
        if let Err(e) = res {
            tracing::warn!(key = %self.key, error = %e, "锁释放失败（将由 TTL 自动过期）");
        }
    }
}

// ── Session ──

/// Redis 会话存储（tower-sessions `SessionStore` 实现）。
///
/// key 前缀 `bff:sess:`；TTL 由 `Record::expiry_date` 推导（Cookie 与服务端过期对齐见 R5）。
#[derive(Clone, Debug)]
pub struct RedisSessionStore {
    pool: RedisPool,
}

impl RedisSessionStore {
    pub fn new(pool: RedisPool) -> Self {
        Self { pool }
    }

    fn key(&self, id: &Id) -> String {
        format!("bff:sess:{}", id)
    }
}

/// 由记录过期时刻推导 TTL（毫秒，至少 1ms）。
fn ttl_ms(record: &Record) -> i64 {
    let delta = record.expiry_date - time::OffsetDateTime::now_utc();
    delta.whole_milliseconds().clamp(1, i64::MAX as i128) as i64
}

fn backend_err(e: redis::RedisError) -> session_store::Error {
    session_store::Error::Backend(e.to_string())
}

#[async_trait]
impl SessionStore for RedisSessionStore {
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        let payload =
            serde_json::to_vec(record).map_err(|e| session_store::Error::Encode(e.to_string()))?;
        let ttl = ttl_ms(record);
        let mut conn = self.pool.conn().await.map_err(backend_err)?;

        // SET NX：ID 冲突时重新生成（与 MemoryStore 的碰撞策略一致）
        for _ in 0..5 {
            let set: Option<String> = redis::cmd("SET")
                .arg(self.key(&record.id))
                .arg(&payload)
                .arg("PX")
                .arg(ttl)
                .arg("NX")
                .query_async(&mut conn)
                .await
                .map_err(backend_err)?;
            if set.is_some() {
                return Ok(());
            }
            record.id = Id::default();
        }
        Err(session_store::Error::Backend(
            "会话 ID 冲突：重试次数耗尽".into(),
        ))
    }

    async fn save(&self, record: &Record) -> session_store::Result<()> {
        let payload =
            serde_json::to_vec(record).map_err(|e| session_store::Error::Encode(e.to_string()))?;
        let ttl = ttl_ms(record);
        let mut conn = self.pool.conn().await.map_err(backend_err)?;
        let res: redis::RedisResult<()> = redis::cmd("SET")
            .arg(self.key(&record.id))
            .arg(&payload)
            .arg("PX")
            .arg(ttl)
            .query_async(&mut conn)
            .await;
        res.map_err(backend_err)
    }

    async fn load(&self, session_id: &Id) -> session_store::Result<Option<Record>> {
        let key = self.key(session_id);
        let mut conn = self.pool.conn().await.map_err(backend_err)?;
        let data: Option<Vec<u8>> = redis::cmd("GET")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .map_err(backend_err)?;

        let Some(bytes) = data else {
            return Ok(None);
        };
        match serde_json::from_slice::<Record>(&bytes) {
            Ok(rec) => Ok(Some(rec)),
            Err(e) => {
                // 损坏数据（版本升级/人工污染）：删除，避免每次请求都报解码错误
                tracing::warn!(%key, error = %e, "会话记录解码失败，已删除");
                let mut conn2 = self.pool.conn().await.map_err(backend_err)?;
                let _: redis::RedisResult<i64> =
                    redis::cmd("DEL").arg(&key).query_async(&mut conn2).await;
                Err(session_store::Error::Decode(e.to_string()))
            }
        }
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        let mut conn = self.pool.conn().await.map_err(backend_err)?;
        let res: redis::RedisResult<i64> = redis::cmd("DEL")
            .arg(self.key(session_id))
            .query_async(&mut conn)
            .await;
        res.map(|_| ()).map_err(backend_err)
    }
}
