//! 配置持久化与多副本收敛（管理端落盘 + 外部变更热重载）。
mod common;

use bff::config::{RouteDef, RouteType, RouteTypeConfig};
use std::sync::Arc;
use std::time::Duration;

fn temp_path(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bff-persist-{}-{}", tag, uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("runtime.yaml")
}

fn demo_route(path: &str) -> RouteDef {
    RouteDef {
        path: path.into(),
        methods: vec!["GET".into()],
        description: "persistence demo".into(),
        auth_required: false,
        route_type: RouteType::Static,
        config: RouteTypeConfig {
            status: Some(200),
            body: Some(serde_json::json!({"ok": true})),
            ..Default::default()
        },
        input_mapping: Default::default(),
        output_mapping: Default::default(),
    }
}

fn cfg_with_persistence(path: &std::path::Path) -> bff::config::AppConfig {
    let mut cfg = common::base_config();
    cfg.persistence.enabled = true;
    cfg.persistence.path = path.to_string_lossy().into_owned();
    cfg.persistence.watch_interval = Duration::from_millis(100);
    cfg
}

/// T1：管理端导入 → 落盘（脱敏）→ 重启（重新解析文件 + 哨兵回填）不丢配置、不泄密钥。
#[tokio::test]
async fn admin_import_persists_and_recovers_without_secret_leak() {
    let path = temp_path("recover");
    let cfg = cfg_with_persistence(&path);
    let state = common::make_state(cfg);
    let admin = common::spawn_admin(state.clone()).await;

    // 模拟“导出 → 修改 → 导入”回环：内容为脱敏配置（admin token = ***）+ 新路由
    let mut import_cfg = state.cfg().sanitized();
    import_cfg.routes.push(demo_route("/persist-demo"));
    let yaml = serde_yaml::to_string(&import_cfg).unwrap();

    let resp = common::test_client()
        .post(format!("{}/admin/api/v1/config/import", admin))
        .header("x-admin-token", "test-admin-token")
        .body(yaml)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "导入应成功");

    // 1) 文件已落盘
    assert!(path.is_file(), "持久化文件应存在");
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("/persist-demo"), "文件应包含新路由");
    // 2) 真实密钥不落盘（admin token 以 *** 哨兵形式写入）
    assert!(
        !content.contains("test-admin-token"),
        "持久化文件不得包含真实管理 token（应为哨兵）"
    );
    assert!(content.contains("***"), "应存在哨兵掩码");

    // 3) 模拟重启加载：解析文件 + 按基础配置回填哨兵 → 配置完整恢复
    let mut overlay: bff::config::AppConfig = serde_yaml::from_str(&content).unwrap();
    overlay.merge_sensitive_secrets(&state.cfg());
    overlay.validate().expect("回填后配置应通过校验");
    assert!(
        overlay.routes.iter().any(|r| r.path == "/persist-demo"),
        "重启后新路由应恢复"
    );
    assert_eq!(
        overlay.admin.auth_token, "test-admin-token",
        "管理 token 应从环境/基础配置回填，而不是 ***"
    );
}

/// T2：外部（另一副本/运维）修改持久化文件 → watcher 热重载生效。
#[tokio::test]
async fn external_file_change_is_hot_reloaded() {
    let path = temp_path("watch");
    let cfg = cfg_with_persistence(&path);
    let state = Arc::new(common::make_state(cfg));

    // 先经管理端写一次（建立“本进程写入哈希”）
    let admin = common::spawn_admin((*state).clone()).await;
    let mut first = state.cfg().sanitized();
    first.routes.push(demo_route("/persist-first"));
    let resp = common::test_client()
        .post(format!("{}/admin/api/v1/config/import", admin))
        .header("x-admin-token", "test-admin-token")
        .body(serde_yaml::to_string(&first).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // 启动 watcher
    let watcher_state = state.clone();
    tokio::spawn(async move { watcher_state.run_config_watcher().await });

    // 外部写入新内容（模拟另一副本）
    let mut external = state.cfg().sanitized();
    external.routes.push(demo_route("/persist-external"));
    std::fs::write(&path, serde_yaml::to_string(&external).unwrap()).unwrap();

    // 轮询等待收敛（watch_interval = 100ms）
    let mut applied = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if state
            .cfg()
            .routes
            .iter()
            .any(|r| r.path == "/persist-external")
        {
            applied = true;
            break;
        }
    }
    assert!(applied, "外部文件变更应在 ~5s 内被热重载");
}

/// T3：持久化关闭时管理端变更不落盘（默认开发/测试行为不变）。
#[tokio::test]
async fn persistence_disabled_writes_nothing() {
    let cfg = common::base_config(); // persistence.enabled = false
    let state = common::make_state(cfg);
    let admin = common::spawn_admin(state.clone()).await;

    let mut import_cfg = state.cfg().sanitized();
    import_cfg.routes.push(demo_route("/no-persist"));
    let resp = common::test_client()
        .post(format!("{}/admin/api/v1/config/import", admin))
        .header("x-admin-token", "test-admin-token")
        .body(serde_yaml::to_string(&import_cfg).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // 默认路径（config/state/runtime.yaml）不应被创建
    assert!(
        !std::path::Path::new("config/state/runtime.yaml").exists(),
        "持久化关闭时不应写文件"
    );
}
