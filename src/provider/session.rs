use crate::config::SessionConfig;
use async_trait::async_trait;
use std::sync::Arc;
use tower_sessions::cookie::SameSite;
use tower_sessions::session::{Id, Record};
use tower_sessions::session_store::{self, SessionStore};
use tower_sessions::SessionManagerLayer;

/// `SessionStore` 的类型擦除包装。
///
/// tower-sessions 的 `SessionManagerLayer<Store>` 与 `Session::new` 需要 `Sized` 的 Store 类型，
/// 而 BFF 需要在运行时选择 memory / redis 后端 → 用本包装统一类型。
pub struct DynSessionStore(Arc<dyn SessionStore>);

impl DynSessionStore {
    pub fn new(store: Arc<dyn SessionStore>) -> Self {
        Self(store)
    }
}

impl std::fmt::Debug for DynSessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynSessionStore").finish_non_exhaustive()
    }
}

impl Clone for DynSessionStore {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

#[async_trait]
impl SessionStore for DynSessionStore {
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        self.0.create(record).await
    }

    async fn save(&self, record: &Record) -> session_store::Result<()> {
        self.0.save(record).await
    }

    async fn load(&self, session_id: &Id) -> session_store::Result<Option<Record>> {
        self.0.load(session_id).await
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        self.0.delete(session_id).await
    }
}

/// 基于共享 store 构造 Session 层（store 同时存入 AppState 供测试/管理端访问）。
pub fn build_layer(
    store: Arc<dyn SessionStore>,
    session: &SessionConfig,
) -> anyhow::Result<SessionManagerLayer<DynSessionStore>> {
    let same_site = match session.same_site.to_ascii_lowercase().as_str() {
        "lax" => SameSite::Lax,
        "strict" => SameSite::Strict,
        "none" => SameSite::None,
        other => anyhow::bail!("非法 same_site 配置: {}", other),
    };
    let layer = SessionManagerLayer::new(DynSessionStore(store))
        .with_name(session.cookie_name.clone())
        .with_secure(session.secure)
        .with_http_only(session.http_only)
        .with_same_site(same_site);
    Ok(layer)
}
