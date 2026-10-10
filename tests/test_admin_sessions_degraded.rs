//! `GET /admin/api/sessions` 的降级语义（§10）：会话存储（Redis）临时故障时，
//! 不能因 `load` 出错而清空整个列表——必须保留内存索引中的 `SessionInfo` 行
//! （`providers`/`sites` 可能为空），仅 record 缺失（`Ok(None)`）才跳过。
mod common;

use async_trait::async_trait;
use bff::state::SessionInfo;
use std::sync::Arc;
use tower_sessions::session::{Id, Record};
use tower_sessions::session_store::{self, SessionStore};

/// 始终返回后端错误的 session store，模拟 Redis 故障。
#[derive(Debug)]
struct FailingSessionStore;

#[async_trait]
impl SessionStore for FailingSessionStore {
    async fn create(&self, _record: &mut Record) -> session_store::Result<()> {
        Err(session_store::Error::Backend("store down".into()))
    }
    async fn save(&self, _record: &Record) -> session_store::Result<()> {
        Err(session_store::Error::Backend("store down".into()))
    }
    async fn load(&self, _session_id: &Id) -> session_store::Result<Option<Record>> {
        Err(session_store::Error::Backend("store down".into()))
    }
    async fn delete(&self, _session_id: &Id) -> session_store::Result<()> {
        Err(session_store::Error::Backend("store down".into()))
    }
}

#[tokio::test]
async fn list_sessions_keeps_index_row_on_store_error() {
    let mut state = common::make_state(common::base_config());
    // 用故障 store 替换内存 store（字段为 pub，测试可直接注入）
    state.session_store = Arc::new(FailingSessionStore);

    let id = Id::default().to_string();
    state.sessions.write().await.insert(
        id.clone(),
        SessionInfo {
            id: id.clone(),
            provider: "pA".into(),
            sub: "user-1".into(),
            created_at: 0,
            last_seen: 0,
            sites: vec![],
            providers: vec![],
        },
    );

    let json = bff::admin::runtime_api::list_sessions(axum::extract::State(state)).await;
    let sessions = json.0["sessions"].as_array().expect("sessions 应为数组");
    assert_eq!(
        sessions.len(),
        1,
        "存储故障时应保留内存索引行，实际: {}",
        json.0
    );
    assert_eq!(sessions[0]["id"], serde_json::json!(id));
    assert_eq!(json.0["count"], 1);
}
