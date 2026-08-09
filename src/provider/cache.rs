use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[async_trait]
pub trait CacheProvider: Send + Sync {
    async fn get(&self, key: &str) -> Option<Vec<u8>>;
    async fn set(&self, key: &str, value: Vec<u8>, ttl: Duration);
    async fn delete(&self, key: &str);
}

/// 内存实现（基于 moka），支持条目级 TTL。
pub struct InMemoryCache {
    inner: moka::future::Cache<String, Vec<u8>>,
    /// 条目级 TTL 覆盖表（key → 过期时刻）
    entry_ttl: Arc<RwLock<HashMap<String, Instant>>>,
    default_ttl: Duration,
}

impl InMemoryCache {
    pub fn new(max_capacity: u64, ttl: Duration) -> Self {
        Self {
            inner: moka::future::Cache::builder()
                .max_capacity(max_capacity)
                .time_to_live(ttl)
                .build(),
            entry_ttl: Arc::new(RwLock::new(HashMap::new())),
            default_ttl: ttl,
        }
    }
}

impl Default for InMemoryCache {
    fn default() -> Self {
        Self::new(10_000, Duration::from_secs(300))
    }
}

#[async_trait]
impl CacheProvider for InMemoryCache {
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        // 检查条目级 TTL 是否过期
        let expired = {
            let ttls = self.entry_ttl.read().await;
            ttls.get(key)
                .map(|&expiry| expiry <= Instant::now())
                .unwrap_or(false)
        };
        if expired {
            self.inner.invalidate(key).await;
            self.entry_ttl.write().await.remove(key);
            return None;
        }
        self.inner.get(key).await
    }

    async fn set(&self, key: &str, value: Vec<u8>, ttl: Duration) {
        // 记录条目级 TTL（与默认 TTL 不同时）
        if ttl != self.default_ttl && ttl > Duration::ZERO {
            self.entry_ttl
                .write()
                .await
                .insert(key.to_string(), Instant::now() + ttl);
        }
        self.inner.insert(key.to_string(), value).await;
    }

    async fn delete(&self, key: &str) {
        self.inner.invalidate(key).await;
        self.entry_ttl.write().await.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_per_entry_ttl() {
        let cache = InMemoryCache::new(100, Duration::from_secs(60));

        // 设置短 TTL 条目
        cache
            .set("short", b"value".to_vec(), Duration::from_millis(50))
            .await;
        // 设置长 TTL 条目
        cache
            .set("long", b"value2".to_vec(), Duration::from_secs(60))
            .await;

        // 短 TTL 未过期时应存在
        assert!(cache.get("short").await.is_some());
        assert!(cache.get("long").await.is_some());

        // 等待短 TTL 过期
        tokio::time::sleep(Duration::from_millis(100)).await;

        // 短 TTL 应已过期
        assert!(cache.get("short").await.is_none());
        // 长 TTL 仍存在
        assert!(cache.get("long").await.is_some());
    }

    #[tokio::test]
    async fn test_delete_clears_ttl() {
        let cache = InMemoryCache::new(100, Duration::from_secs(60));
        cache
            .set("key", b"val".to_vec(), Duration::from_secs(10))
            .await;
        cache.delete("key").await;
        // 删除后 get 应为 None
        assert!(cache.get("key").await.is_none());
    }
}
