use bff::config::AppConfig;
use bff::state::AppState;
use std::path::PathBuf;
use tokio::signal;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_dir = std::env::var("BFF_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config"));
    let config = AppConfig::load(&config_dir)?;

    // O3：OTel 追踪导出（telemetry.otlp_endpoint 为空则完全禁用）
    let telemetry = bff::telemetry::init(&config.telemetry)?;

    // JSON 结构化日志（RUST_LOG 控制级别）；启用遥测时叠加 OTel span 导出层
    let fmt_layer = tracing_subscriber::fmt::layer().json();
    let registry = tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(fmt_layer);
    match &telemetry {
        Some(handle) => {
            let otel_layer = tracing_opentelemetry::layer().with_tracer(handle.tracer());
            registry.with(otel_layer).init();
        }
        None => registry.init(),
    }

    let business_port = config.server.business_port;
    let admin_port = config.server.admin_port;

    let state = AppState::new(config)?;
    // 启动依赖自检：Redis 启用时 PING 一次（fail-fast）
    state.verify_dependencies().await?;
    tracing::info!(business_port, admin_port, "BFF 启动中");

    // R5：后台会话索引 GC（清理 store 中已过期的会话条目，防内存无界增长与列表失真）
    {
        let gc_state = std::sync::Arc::new(state.clone());
        let gc_interval = state.cfg().session.gc_interval;
        tokio::spawn(async move { gc_state.run_session_gc(gc_interval).await });
    }

    // P0-4：外部配置变更轮询（多副本共享存储时收敛管理端变更）
    {
        let watch_state = std::sync::Arc::new(state.clone());
        tokio::spawn(async move { watch_state.run_config_watcher().await });
    }

    let business_router = bff::server::business::build_business_router(state.clone())?;
    let admin_router = bff::server::admin::build_admin_router(state)?;

    let business_addr = std::net::SocketAddr::from(([0, 0, 0, 0], business_port));
    let admin_addr = std::net::SocketAddr::from(([0, 0, 0, 0], admin_port));

    let business_listener = tokio::net::TcpListener::bind(business_addr).await?;
    let admin_listener = tokio::net::TcpListener::bind(admin_addr).await?;
    tracing::info!(%business_addr, "业务端口已监听");
    tracing::info!(%admin_addr, "管理端口已监听");

    // R4：单一信号源（SIGTERM/SIGINT 各注册一次），经 watch 广播给两个服务。
    // 原实现三处独立注册信号 + 固定 sleep(2s) 后直接退出（硬杀在途请求/WS 连接）。
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        tracing::info!("收到终止信号，开始优雅关闭（停止接受新连接）...");
        let _ = shutdown_tx.send(true);
    });

    let business = axum::serve(
        business_listener,
        business_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(watch_shutdown(shutdown_rx.clone()));

    let admin = axum::serve(
        admin_listener,
        admin_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(watch_shutdown(shutdown_rx.clone()));

    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move { business.await.map_err(|e| e.to_string()) });
    servers.spawn(async move { admin.await.map_err(|e| e.to_string()) });

    // 等待首个服务结束（正常情况下由信号触发的优雅关闭使其先后结束）
    if let Some(res) = servers.join_next().await {
        match res {
            Ok(Ok(())) => tracing::info!("一个服务已优雅退出"),
            Ok(Err(e)) => tracing::error!(error = %e, "服务异常退出"),
            Err(e) => tracing::error!(error = %e, "服务任务异常"),
        }
    }

    // 排空其余在途请求/连接：显式截止时间（与 K8s terminationGracePeriod 对齐），
    // 替代原先固定 sleep(2s) 后强制退出的行为。
    const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    let drain = async { while servers.join_next().await.is_some() {} };
    if tokio::time::timeout(DRAIN_TIMEOUT, drain).await.is_err() {
        tracing::warn!("等待在途请求排空超时（30s），强制退出");
    }

    // O3：flush + 关停 OTel（导出队列中的尾部落 span），再记录最终日志。
    // shutdown_async：SDK 的阻塞式关停在 current_thread 运行时会死锁，
    // 统一放入阻塞线程池执行（详见 telemetry::TelemetryHandle::shutdown_async）。
    if let Some(handle) = telemetry {
        handle.shutdown_async().await;
    }
    tracing::info!("BFF 已关闭");
    Ok(())
}

/// 等待关闭信号（SIGINT/SIGTERM 任一）。
async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("无法注册 Ctrl+C 处理器");
    };
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("无法注册 SIGTERM 处理器")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

/// 将 watch 通道转换为 `with_graceful_shutdown` 所需的 future。
async fn watch_shutdown(mut rx: tokio::sync::watch::Receiver<bool>) {
    if *rx.borrow() {
        return;
    }
    let _ = rx.changed().await;
}
