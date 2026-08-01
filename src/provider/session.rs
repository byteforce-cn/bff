use crate::config::SessionConfig;
use tower_sessions::cookie::SameSite;
use tower_sessions::{MemoryStore, SessionManagerLayer};

/// 基于共享 store 构造 Session 层（store 同时存入 AppState 供测试/管理端访问）。
pub fn build_layer(
    store: MemoryStore,
    session: &SessionConfig,
) -> anyhow::Result<SessionManagerLayer<MemoryStore>> {
    let same_site = match session.same_site.to_ascii_lowercase().as_str() {
        "lax" => SameSite::Lax,
        "strict" => SameSite::Strict,
        "none" => SameSite::None,
        other => anyhow::bail!("非法 same_site 配置: {}", other),
    };
    let layer = SessionManagerLayer::new(store)
        .with_name(session.cookie_name.clone())
        .with_secure(session.secure)
        .with_http_only(session.http_only)
        .with_same_site(same_site);
    Ok(layer)
}
