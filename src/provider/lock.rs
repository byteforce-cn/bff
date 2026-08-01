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

/// 内存实现：按 key 维护互斥锁
pub struct InMemoryLock {
    locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl InMemoryLock {
    pub fn new() -> Self {
        Self {
            locks: Arc::new(Mutex::new(HashMap::new())),
        }
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
            let mut map = self.locks.lock().await;
            map.entry(key.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        // lock_owned 产出 'static 的 OwnedMutexGuard，可安全装箱
        let guard = tokio::time::timeout(wait_timeout, mutex.lock_owned())
            .await
            .ok()?;
        Some(Box::new(InMemoryLockGuard { _guard: guard }))
    }
}

struct InMemoryLockGuard {
    _guard: OwnedMutexGuard<()>,
}

#[async_trait]
impl LockGuard for InMemoryLockGuard {
    async fn release(self: Box<Self>) {
        // drop 时自动释放
    }
}
