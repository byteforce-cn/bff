//! Task 10：多站点测试夹具与共享会话回归（§7.1/§7.2、§6.1）。
//!
//! 不依赖 HTTP router，直接操作 `AppState` + `Session` 验证：
//! 站点句柄拆分、同一 store 的跨站点视图可见性、legacy 旧键迁移与多站点校验联动。

mod common;

use bff::oidc::StoredTokens;
use bff::state::AppState;
use tower_sessions::session::Id;

/// 从 `BFF_SESSION={id}` Cookie 头值解析会话 id。
fn session_id(cookie: &str) -> Id {
    cookie
        .strip_prefix("BFF_SESSION=")
        .expect("会话 Cookie 应以 BFF_SESSION= 开头")
        .parse()
        .expect("会话 id 应可解析")
}

/// 多站点配置 → 两个独立站点句柄（共享 `default` profile），legacy 配置 → 单个 default。
#[tokio::test]
async fn site_handles_returns_two_separate_handles() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let state = common::make_state(common::multisite_config(&idp_a, &idp_b));

    let handles = state.site_handles().expect("构建多站点句柄");
    assert_eq!(handles.len(), 2);
    assert_eq!(handles[0].name, "app1");
    assert_eq!(handles[1].name, "app2");
    assert_eq!(handles[0].port, 8081);
    assert_eq!(handles[1].port, 8082);
    assert!(handles.iter().all(|h| h.session_profile == "default"));
    assert!(
        handles.iter().all(|h| !h.legacy),
        "显式多站点句柄不得标记 legacy"
    );

    // legacy 配置（无 sites）→ 合成的 default 站点句柄
    let legacy = common::make_state(common::base_config());
    let handles = legacy.site_handles().expect("构建 legacy 句柄");
    assert_eq!(handles.len(), 1);
    assert_eq!(handles[0].name, "default");
    assert!(handles[0].legacy, "无 sites 的配置应合成 legacy 站点");
}

/// 同一份 SessionStore、同一会话 id：站点 A 写入的令牌对 A 可见、对 B 不可见（§7.2）。
#[tokio::test]
async fn same_store_session_is_visible_across_site_views() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let state = common::make_state(common::multisite_config(&idp_a, &idp_b));

    // 站点 A 的会话写入 pA 令牌并落库
    let session = common::session_for(&state, None);
    let tokens = common::write_tokens(&session, "pA").await;
    assert_eq!(tokens.provider, "pA");
    session.save().await.expect("保存会话");

    // 复用同一 store、同一 id 新建 Session（模拟站点 B 收到同一 Cookie）
    let reopened = common::session_for(&state, Some(session.id().expect("会话应有 id")));
    let view_a = state.site_view("app1").expect("app1 视图");
    let view_b = state.site_view("app2").expect("app2 视图");

    let visible = view_a
        .current_tokens(&reopened)
        .await
        .expect("app1 应可见 pA 令牌");
    assert_eq!(visible.provider, "pA");
    assert!(
        view_b.current_tokens(&reopened).await.is_none(),
        "app2 只允许 pB，不得看到 pA 令牌"
    );
}

/// legacy 合成 default 站点首次读取时把 `oidc:current_provider` 迁移为站点键并删除旧键（§7.2）。
#[tokio::test]
async fn legacy_key_migrates_on_first_read() {
    let mut cfg = common::base_config();
    cfg.oidc.providers.push(common::synthetic_provider_cfg());
    let state = common::make_state(cfg);

    // create_session_with_tokens 写入旧键 `oidc:current_provider` + mock 令牌
    let tokens = StoredTokens::new(
        "mock",
        "test-user",
        "test-access-token",
        Some("test-refresh-token"),
        None,
        3600,
    )
    .expect("构造 StoredTokens 失败");
    let cookie = common::create_session_with_tokens(&state, &tokens).await;
    let session = common::session_for(&state, Some(session_id(&cookie)));

    let view = state.site_view("default").expect("legacy default 视图");
    assert!(view.legacy);
    assert_eq!(
        view.current_provider(&session).await.as_deref(),
        Some("mock"),
        "旧键中的 provider 应被识别"
    );

    let old: Option<String> = session.get("oidc:current_provider").await.unwrap();
    assert!(old.is_none(), "旧键应被删除");
    let migrated: Option<String> = session.get("oidc:default:current_provider").await.unwrap();
    assert_eq!(migrated.as_deref(), Some("mock"), "应写入站点键");
}

/// `multisite_config` 在 dev 语义下通过校验并共享 `default` profile；
/// 两站点绑定同一 provider（同 profile）且未确认 `shared_across_sites` → 启动失败（§5.4 第 10 条）。
#[tokio::test]
async fn multisite_config_validates_and_keeps_profiles_isolated() {
    let idp_a = common::spawn_mock_oidc_provider().await;
    let idp_b = common::spawn_mock_oidc_provider().await;
    let cfg = common::multisite_config(&idp_a, &idp_b);

    let state = AppState::new(cfg.clone()).expect("dev 语义多站点配置应通过校验");
    assert!(state.session_layers.contains_key("default"));
    assert_eq!(state.session_layers.len(), 1, "两站点共享 default profile");
    let handles = state.site_handles().expect("构建句柄");
    assert!(handles.iter().all(|h| h.session_profile == "default"));

    // 让两站点都绑定 pA：同一 profile 下 provider 被多站点共享 → 必须显式确认
    let mut bad = cfg;
    for site in &mut bad.sites {
        site.oidc.default_provider = "pA".into();
        site.oidc.allowed_providers = Some(vec!["pA".into()]);
    }
    let err = AppState::new(bad)
        .err()
        .expect("站点间共享 provider 应被拒绝");
    assert!(
        err.to_string().contains("shared_across_sites"),
        "错误应提示 shared_across_sites，实际: {err}"
    );
}
