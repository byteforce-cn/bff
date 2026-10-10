use bff::config::AppConfig;
use bff::server::serve::{run_servers, watch_shutdown, ServerFuture};
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

    // OTel 追踪导出（telemetry.otlp_endpoint 为空则完全禁用）
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

    let admin_port = config.server.admin_port;

    let state = AppState::new(config)?;
    // 启动依赖自检：Redis 启用时 PING 一次（fail-fast）
    state.verify_dependencies().await?;
    tracing::info!(admin_port, "BFF 启动中");

    // 后台会话索引 GC（清理 store 中已过期的会话条目，防内存无界增长与列表失真）
    {
        let gc_state = std::sync::Arc::new(state.clone());
        let gc_interval = state.cfg().session.gc_interval;
        tokio::spawn(async move { gc_state.run_session_gc(gc_interval).await });
    }

    // 外部配置变更轮询（多副本共享存储时收敛管理端变更）
    {
        let watch_state = std::sync::Arc::new(state.clone());
        tokio::spawn(async move { watch_state.run_config_watcher().await });
    }

    let handles = state.site_handles()?;

    // 单一信号源（SIGTERM/SIGINT 各注册一次），经 watch 广播给全部 listener。
    // 原实现三处独立注册信号 + 固定 sleep(2s) 后直接退出（硬杀在途请求/WS 连接）。
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let signal_tx = shutdown_tx.clone();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        tracing::info!("收到终止信号，开始优雅关闭（停止接受新连接）...");
        let _ = signal_tx.send(true);
    });

    // §6.1：逐站点 bind + 构建 router（每个 listener 一个）；任一 bind 失败即启动失败。
    let mut servers: Vec<(String, ServerFuture)> = Vec::with_capacity(handles.len() + 1);
    for handle in handles {
        let router = bff::server::business::build_site_router(state.clone(), handle.clone())?;
        let listener = tokio::net::TcpListener::bind((handle.bind.as_str(), handle.port)).await?;
        tracing::info!(site = %handle.name, bind = %handle.bind, port = handle.port, "业务端口已监听");
        let name = handle.name.clone();
        let shutdown = shutdown_rx.clone();
        servers.push((
            name.clone(),
            Box::pin(async move {
                axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .with_graceful_shutdown(watch_shutdown(shutdown))
                .await
                .map_err(|e| anyhow::anyhow!("站点 {name} listener 异常: {e}"))
            }),
        ));
    }

    // 管理端 listener（绑定 0.0.0.0，与 legacy 行为一致）。
    let admin_router = bff::server::admin::build_admin_router(state)?;
    let admin_addr = std::net::SocketAddr::from(([0, 0, 0, 0], admin_port));
    let admin_listener = tokio::net::TcpListener::bind(admin_addr).await?;
    tracing::info!(%admin_addr, "管理端口已监听");
    {
        let shutdown = shutdown_rx.clone();
        servers.push((
            "admin".to_string(),
            Box::pin(async move {
                axum::serve(
                    admin_listener,
                    admin_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .with_graceful_shutdown(watch_shutdown(shutdown))
                .await
                .map_err(|e| anyhow::anyhow!("admin listener 异常: {e}"))
            }),
        ));
    }

    // §6.1 / §12：首个结束的 listener 决定语义——异常退出即触发全局关闭。
    let result = run_servers(servers, shutdown_tx, shutdown_rx).await;

    // flush + 关停 OTel（导出队列中的尾部落 span），再记录最终日志。
    // shutdown_async：SDK 的阻塞式关停在 current_thread 运行时会死锁，
    // 统一放入阻塞线程池执行（详见 telemetry::TelemetryHandle::shutdown_async）。
    if let Some(handle) = telemetry {
        handle.shutdown_async().await;
    }
    match result {
        Ok(()) => {
            tracing::info!("BFF 已关闭");
            Ok(())
        }
        Err(e) => {
            tracing::error!(error = %e, "BFF 异常退出");
            Err(e)
        }
    }
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
