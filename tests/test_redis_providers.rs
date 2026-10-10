//! Redis provider 集成测试：Cache / Lock / Session 与跨实例会话共享。
//!
//! 需要可达的 Redis；未设置 `BFF_TEST_REDIS_URL` 时自动跳过（CI 无 Redis 不红）。
//!
//! 本地运行（Docker）：
//! ```bash
//! docker run -d --name bff-redis -p 6379:6379 redis:7-alpine
//! BFF_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test --test test_redis_providers
//! ```

mod common;

use bff::provider::redis::{RedisCache, RedisLock, RedisPool, RedisSessionStore};
use bff::provider::{CacheProvider, LockProvider};
use std::time::Duration;
use tower_sessions::session::{Id, Record};
use tower_sessions::session_store::SessionStore;

fn redis_url() -> Option<String> {
    std::env::var("BFF_TEST_REDIS_URL").ok()
}

fn test_pool() -> Option<RedisPool> {
    redis_url().map(|url| RedisPool::new(&url).expect("RedisPool 构建失败"))
}

/// Cache：set/get/delete 回环 + 条目级 TTL 生效。
#[tokio::test]
async fn cache_roundtrip_and_ttl() {
    let Some(pool) = test_pool() else {
        return;
    };
    let cache = RedisCache::new(pool);
    let key = format!("bff:test:cache:{}", uuid::Uuid::new_v4());

    assert_eq!(cache.get(&key).await, None, "初始应未命中");
    cache
        .set(&key, b"v1".to_vec(), Duration::from_secs(60))
        .await;
    assert_eq!(cache.get(&key).await, Some(b"v1".to_vec()));

    // 覆盖写
    cache
        .set(&key, b"v2".to_vec(), Duration::from_secs(60))
        .await;
    assert_eq!(cache.get(&key).await, Some(b"v2".to_vec()));

    // TTL 过期
    let short = format!("{}:ttl", key);
    cache
        .set(&short, b"x".to_vec(), Duration::from_millis(60))
        .await;
    assert_eq!(cache.get(&short).await, Some(b"x".to_vec()));
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(cache.get(&short).await, None, "TTL 过期后应未命中");

    cache.delete(&key).await;
    assert_eq!(cache.get(&key).await, None, "删除后应未命中");
}

/// Lock：互斥性（跨实例）+ 释放后可重新获取 + 等待超时返回 None。
#[tokio::test]
async fn lock_mutual_exclusion_and_release() {
    let Some(pool) = test_pool() else {
        return;
    };
    let lock_a = RedisLock::new(pool.clone());
    let lock_b = RedisLock::new(pool);
    let key = format!("bff:test:lock:{}", uuid::Uuid::new_v4());

    let guard_a = lock_a
        .acquire(&key, Duration::from_millis(100), Duration::from_secs(5))
        .await;
    assert!(guard_a.is_some(), "首次获取应成功");

    // 另一实例在等待窗口内拿不到锁
    let guard_b = lock_b
        .acquire(&key, Duration::from_millis(300), Duration::from_secs(5))
        .await;
    assert!(guard_b.is_none(), "互斥锁不应被两个实例同时持有");

    // 释放后另一实例可获取
    guard_a.unwrap().release().await;
    let guard_c = lock_b
        .acquire(&key, Duration::from_millis(500), Duration::from_secs(5))
        .await;
    assert!(guard_c.is_some(), "释放后应可重新获取");
    guard_c.unwrap().release().await;
}

/// SessionStore：create/load/save/delete 回环 + 跨实例可见。
#[tokio::test]
async fn session_store_shared_across_instances() {
    let Some(pool) = test_pool() else {
        return;
    };
    let store_a = RedisSessionStore::new(pool.clone());
    let store_b = RedisSessionStore::new(pool);

    let mut record = Record {
        id: Id::default(),
        data: Default::default(),
        expiry_date: time::OffsetDateTime::now_utc() + time::Duration::seconds(300),
    };
    let id = record.id;

    store_a.create(&mut record).await.expect("create 应成功");
    let loaded = store_b
        .load(&id)
        .await
        .expect("load 应成功")
        .expect("另一实例应能读到会话");
    assert_eq!(loaded.id, id);
    assert_eq!(loaded, record);

    store_b.delete(&id).await.expect("delete 应成功");
    assert!(store_a.load(&id).await.expect("load 应成功").is_none());
}

/// 端到端：两个 BFF 实例共享 Redis 会话——实例 A 登录的 Cookie 在实例 B 有效。
#[tokio::test]
async fn session_cookie_works_across_two_bff_instances() {
    let Some(url) = redis_url() else {
        return;
    };
    let mut cfg = common::base_config();
    cfg.provider.session_store = "redis".into();
    cfg.provider.cache = "redis".into();
    cfg.provider.redis_url = url;
    // 站点化后“已登录会话”要求 provider ∈ 站点白名单（§7.2）
    cfg.oidc.providers.push(common::synthetic_provider_cfg());

    // 实例 A：生成已登录会话
    let state_a = common::make_state(cfg.clone());
    let _bff_a = common::spawn_business(state_a.clone()).await;
    let cookie = common::login_cookie(&state_a).await;

    // 实例 B：独立进程内状态，仅共享 Redis
    let state_b = common::make_state(cfg);
    let bff_b = common::spawn_business(state_b).await;

    let resp = common::test_client()
        .get(format!("{}/api/session", bff_b))
        .header("cookie", &cookie)
        .send()
        .await
        .expect("请求失败");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["logged_in"], true,
        "会话应跨实例共享（Redis SessionStore）"
    );
}
