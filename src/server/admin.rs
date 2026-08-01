//! 管理端口（8443）：仅挂载管理路由 + IP 白名单。
use crate::state::AppState;
use axum::Router;

pub fn build_admin_router(state: AppState) -> anyhow::Result<Router> {
    crate::admin::build_admin_router(state)
}
