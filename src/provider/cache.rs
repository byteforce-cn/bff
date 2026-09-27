use async_trait::async_trait;
use moka::future::Cache;
use moka::Expiry;
use std::time::{Duration, Instant};

#[async_trait]
pub trait CacheProvider: Send + Sync {
    async fn get(&self, key: &str) -> Option<Vec<u8>>;
    async fn set(&self, key: &str, value: Vec<u8>, ttl: Duration);
    async fn delete(&self, key: &str);

    /// 删除以 `prefix` 开头的全部键（R17：登出时按 session 前缀清理交换缓存）。
    /// 后端不支持时返回 0。
    async fn delete_prefix(&self, _prefix: &str) -> usize {
        0
    }
}

/// 缓存条目：值 + 条目级 TTL。
///
/// R14：原实现把条目级 TTL 放在旁挂的 `HashMap` 里（moka 容量约束对它无效，
/// 以非默认 TTL 写入后不再被读的 key 会永久残留）。改为把 TTL 随值存入 moka，
/// 用 `Expiry` 策略在条目创建/更新时设定过期时间，容量约束重新生效。
#[derive(Clone, Debug)]
struct Entry {
    value: Vec<u8>,
    ttl: Duration,
}

struct EntryExpiry;

impl Expiry<String, Entry> for EntryExpiry {
    fn expire_after_create(&self, _key: &String, value: &Entry, _now: Instant) -> Option<Duration> {
        Some(value.ttl)
    }

    fn expire_after_update(
        &self,
        _key: &String,
        value: &Entry,
        _now: Instant,
        _current_duration: Option<Duration>,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

/// 内存实现（基于 moka），容量受限 + 条目级 TTL。
pub struct InMemoryCache {
    inner: Cache<String, Entry>,
}

impl InMemoryCache {
    pub fn new(max_capacity: u64, _default_ttl: Duration) -> Self {
        Self {
            inner: Cache::builder()
                .max_capacity(max_capacity)
                .expire_after(EntryExpiry)
                .build(),
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
        self.inner.get(key).await.map(|e| e.value)
    }

    async fn set(&self, key: &str, value: Vec<u8>, ttl: Duration) {
        if ttl == Duration::ZERO {
            // TTL 为零视为“不缓存”（与 Redis `PX 0` 非法保持一致的语义）
            return;
        }
        self.inner
            .insert(key.to_string(), Entry { value, ttl })
            .await;
    }

    async fn delete(&self, key: &str) {
        self.inner.invalidate(key).await;
    }

    async fn delete_prefix(&self, prefix: &str) -> usize {
        // moka 无前缀扫描接口；调用频率极低（登出/会话撤销），O(n) 可接受。
        self.inner.run_pending_tasks().await;
        let keys: Vec<String> = self
            .inner
            .iter()
            .map(|(k, _)| k.to_string())
            .filter(|k| k.starts_with(prefix))
            .collect();
        let mut removed = 0usize;
        for k in keys {
            self.inner.invalidate(&k).await;
            removed += 1;
        }
        removed
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

    #[tokio::test]
    async fn test_delete_prefix() {
        let cache = InMemoryCache::new(100, Duration::from_secs(60));
        cache
            .set("bff:te:s1:a", b"1".to_vec(), Duration::from_secs(60))
            .await;
        cache
            .set("bff:te:s1:b", b"2".to_vec(), Duration::from_secs(60))
            .await;
        cache
            .set("bff:te:s2:a", b"3".to_vec(), Duration::from_secs(60))
            .await;
        let removed = cache.delete_prefix("bff:te:s1:").await;
        assert_eq!(removed, 2);
        assert!(cache.get("bff:te:s1:a").await.is_none());
        assert!(cache.get("bff:te:s2:a").await.is_some());
    }

    #[tokio::test]
    async fn test_zero_ttl_not_stored() {
        let cache = InMemoryCache::new(100, Duration::from_secs(60));
        cache.set("k", b"v".to_vec(), Duration::ZERO).await;
        assert!(cache.get("k").await.is_none());
    }
}
