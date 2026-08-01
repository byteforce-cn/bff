use bff::config::AppConfig;
use bff::state::AppState;
use std::path::PathBuf;
use tokio::signal;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // JSON 结构化日志，可用 RUST_LOG 控制级别
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .json()
        .init();

    let config_dir = std::env::var("BFF_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config"));
    let config = AppConfig::load(&config_dir)?;
    let business_port = config.server.business_port;
    let admin_port = config.server.admin_port;

    let state = AppState::new(config)?;
    tracing::info!(business_port, admin_port, "BFF 启动中");

    let business_router = bff::server::business::build_business_router(state.clone())?;
    let admin_router = bff::server::admin::build_admin_router(state)?;

    let business_addr = std::net::SocketAddr::from(([0, 0, 0, 0], business_port));
    let admin_addr = std::net::SocketAddr::from(([0, 0, 0, 0], admin_port));

    let business_listener = tokio::net::TcpListener::bind(business_addr).await?;
    let admin_listener = tokio::net::TcpListener::bind(admin_addr).await?;
    tracing::info!(%business_addr, "业务端口已监听");
    tracing::info!(%admin_addr, "管理端口已监听");

    let shutdown = graceful_shutdown_signal();

    let business = axum::serve(
        business_listener,
        business_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal());

    let admin = axum::serve(
        admin_listener,
        admin_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal());

    // 两个服务并行运行，任意一个退出或收到信号即关闭
    tokio::select! {
        r = business => tracing::error!(?r, "业务服务退出"),
        r = admin => tracing::error!(?r, "管理服务退出"),
        _ = shutdown => tracing::info!("收到退出信号，正在优雅关闭"),
    }

    // 等待飞行中的请求完成（最长 30 秒）
    tracing::info!("等待飞行中请求完成（最长 30 秒）...");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    tracing::info!("BFF 已关闭");
    Ok(())
}

/// 创建优雅关闭信号：收到 SIGTERM/SIGINT 时触发。
fn graceful_shutdown_signal() -> tokio::sync::oneshot::Receiver<()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let ctrl_c = async {
            signal::ctrl_c()
                .await
                .expect("无法注册 Ctrl+C 处理器");
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
        tracing::info!("收到终止信号，开始优雅关闭...");
        let _ = tx.send(());
    });
    rx
}

/// 提取两个 future 都能用的 shutdown signal。
fn shutdown_signal() -> impl std::future::Future<Output = ()> {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("无法注册 Ctrl+C 处理器");
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

    async {
        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }
    }
}
