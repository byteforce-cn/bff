use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, OwnedMutexGuard};

#[async_trait]
pub trait LockProvider: Send + Sync {
    /// 尝试获取锁。`wait_timeout` 内未获得则返回 None；`hold_ttl` 供分布式实现设置租约。
    async fn acquire(
        &self,
        key: &str,
        wait_timeout: Duration,
        hold_ttl: Duration,
    ) -> Option<Box<dyn LockGuard>>;
}

#[async_trait]
pub trait LockGuard: Send + Sync {
    async fn release(self: Box<Self>);
}

/// 锁表项：互斥体 + 引用计数。
///
/// R14：原实现 `entry()` 后从不移除，键空间（per-IP 限流桶、刷新锁、
/// exchange single-flight 等）由业务量/攻击流量驱动 → 无界增长。
/// 现在按引用计数回收：无等待者/持有者时从表中移除。
struct LockEntry {
    mutex: Arc<Mutex<()>>,
    refs: usize,
}

/// 内存实现：按 key 维护互斥锁（引用计数自动回收）。
pub struct InMemoryLock {
    /// std Mutex：仅保护极短的 map 操作，且允许在 Guard Drop 中同步清理
    locks: Arc<std::sync::Mutex<HashMap<String, LockEntry>>>,
}

impl InMemoryLock {
    pub fn new() -> Self {
        Self {
            locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// 引用计数 -1；归零则移除表项（表项可能在等待期间已被清理，忽略即可）。
    fn release_ref(&self, key: &str) {
        let mut map = self.locks.lock().expect("锁表损坏");
        if let Some(entry) = map.get_mut(key) {
            entry.refs = entry.refs.saturating_sub(1);
            if entry.refs == 0 {
                map.remove(key);
            }
        }
    }

    #[cfg(test)]
    pub fn tracked_keys(&self) -> usize {
        self.locks.lock().expect("锁表损坏").len()
    }
}

impl Default for InMemoryLock {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LockProvider for InMemoryLock {
    async fn acquire(
        &self,
        key: &str,
        wait_timeout: Duration,
        _hold_ttl: Duration,
    ) -> Option<Box<dyn LockGuard>> {
        let mutex = {
            let mut map = self.locks.lock().expect("锁表损坏");
            let entry = map.entry(key.to_string()).or_insert_with(|| LockEntry {
                mutex: Arc::new(Mutex::new(())),
                refs: 0,
            });
            entry.refs += 1;
            entry.mutex.clone()
        };
        // lock_owned 产出 'static 的 OwnedMutexGuard，可安全装箱
        let guard = match tokio::time::timeout(wait_timeout, mutex.lock_owned()).await {
            Ok(g) => g,
            Err(_) => {
                // 等待超时：调用方拿不到 Guard，此处回收引用计数
                self.release_ref(key);
                return None;
            }
        };
        Some(Box::new(InMemoryLockGuard {
            _guard: guard,
            inner: self.locks.clone(),
            key: key.to_string(),
            released: false,
        }))
    }
}

struct InMemoryLockGuard {
    _guard: OwnedMutexGuard<()>,
    inner: Arc<std::sync::Mutex<HashMap<String, LockEntry>>>,
    key: String,
    released: bool,
}

impl InMemoryLockGuard {
    fn release_ref(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let mut map = self.inner.lock().expect("锁表损坏");
        if let Some(entry) = map.get_mut(&self.key) {
            entry.refs = entry.refs.saturating_sub(1);
            if entry.refs == 0 {
                map.remove(&self.key);
            }
        }
    }
}

#[async_trait]
impl LockGuard for InMemoryLockGuard {
    async fn release(mut self: Box<Self>) {
        self.release_ref();
        // OwnedMutexGuard 随 drop 自动释放
    }
}

impl Drop for InMemoryLockGuard {
    fn drop(&mut self) {
        // 调用方未显式 release（或中途 drop）时兜底回收，防表项泄漏
        self.release_ref();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mutex_exclusion() {
        let lock = InMemoryLock::new();
        let g1 = lock
            .acquire("k", Duration::from_millis(50), Duration::from_secs(1))
            .await;
        assert!(g1.is_some());
        // 持锁期间第二个获取者应在 wait_timeout 后失败
        let g2 = lock
            .acquire("k", Duration::from_millis(50), Duration::from_secs(1))
            .await;
        assert!(g2.is_none());
        g1.unwrap().release().await;
        // 释放后可再次获取
        let g3 = lock
            .acquire("k", Duration::from_millis(50), Duration::from_secs(1))
            .await;
        assert!(g3.is_some());
    }

    #[tokio::test]
    async fn test_table_cleanup_after_release() {
        let lock = InMemoryLock::new();
        for i in 0..100 {
            let g = lock
                .acquire(
                    &format!("key-{}", i),
                    Duration::from_millis(50),
                    Duration::from_secs(1),
                )
                .await
                .unwrap();
            g.release().await;
        }
        // R14：全部释放后锁表应回到空（引用计数回收）
        assert_eq!(lock.tracked_keys(), 0);
    }

    #[tokio::test]
    async fn test_table_cleanup_on_drop() {
        let lock = InMemoryLock::new();
        {
            let _g = lock
                .acquire(
                    "drop-key",
                    Duration::from_millis(50),
                    Duration::from_secs(1),
                )
                .await
                .unwrap();
            assert_eq!(lock.tracked_keys(), 1);
        }
        assert_eq!(lock.tracked_keys(), 0);
    }

    #[tokio::test]
    async fn test_timeout_cleans_up() {
        let lock = InMemoryLock::new();
        let g1 = lock
            .acquire(
                "contended",
                Duration::from_millis(50),
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        // 第二个获取者超时 → 不应留下引用计数残留
        let g2 = lock
            .acquire(
                "contended",
                Duration::from_millis(20),
                Duration::from_secs(1),
            )
            .await;
        assert!(g2.is_none());
        g1.release().await;
        assert_eq!(lock.tracked_keys(), 0);
    }
}
