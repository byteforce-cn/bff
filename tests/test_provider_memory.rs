//! 场景 9：内存 Provider 正确性。
mod common;

use bff::provider::{CacheProvider, InMemoryCache, InMemoryLock, LockProvider};
use std::time::Duration;

#[tokio::test]
async fn cache_set_get_delete_and_expiry() {
    let cache = InMemoryCache::new(100, Duration::from_millis(120));
    cache
        .set("k", b"v".to_vec(), Duration::from_millis(120))
        .await;
    assert_eq!(cache.get("k").await, Some(b"v".to_vec()));

    // 过期失效
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(cache.get("k").await, None);

    // 删除
    cache.set("k2", b"v2".to_vec(), Duration::from_secs(60)).await;
    cache.delete("k2").await;
    assert_eq!(cache.get("k2").await, None);
}

#[tokio::test]
async fn lock_is_mutually_exclusive() {
    let lock = InMemoryLock::new();
    let guard = lock
        .acquire("k", Duration::from_millis(100), Duration::from_secs(5))
        .await
        .expect("首次应获得锁");

    // 同 key 竞争：等待超时后失败
    let second = lock
        .acquire("k", Duration::from_millis(80), Duration::from_secs(5))
        .await;
    assert!(second.is_none(), "持锁期间不应再次获得");

    // 释放后可再获得
    guard.release().await;
    let third = lock
        .acquire("k", Duration::from_millis(200), Duration::from_secs(5))
        .await;
    assert!(third.is_some(), "释放后应可获得");

    // 不同 key 互不影响
    let other = lock
        .acquire("other", Duration::from_millis(50), Duration::from_secs(5))
        .await;
    assert!(other.is_some());
}

#[tokio::test]
async fn memory_session_is_lost_after_store_drop() {
    use tower_sessions::{Session, SessionStore};
    let store = tower_sessions::MemoryStore::default();

    // 写入会话
    let session = Session::new(None, std::sync::Arc::new(store.clone()), None);
    session.insert("k", "v").await.unwrap();
    session.save().await.unwrap();
    let id = session.id().unwrap();

    // 正常读取
    assert!(store.load(&id).await.unwrap().is_some());

    // 模拟重启：新 store 中会话不存在 → 需重新登录
    let fresh_store = tower_sessions::MemoryStore::default();
    assert!(fresh_store.load(&id).await.unwrap().is_none());
}
