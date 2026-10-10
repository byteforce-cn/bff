//! §6.1 / §12：多 listener 编排与异常退出语义。
//!
//! 全部 server future（每站点业务 listener + 管理端 listener）注入同一个
//! shutdown watch 通道。任一 listener 提前退出——返回 `Err(_)`，或在**未请求关闭**
//! 时返回 `Ok(())`——都视为异常：向全通道广播关闭、排空其余任务后返回错误，
//! 由进程退出交给 K8s 重启。绝不允许“部分站点静默不可用而 Pod 仍 Ready”。

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinSet;

/// 单个 server future：绑定 listener 后的
/// `axum::serve(...).with_graceful_shutdown(...)`。
pub type ServerFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;

/// 等待在途请求 / 连接排空的截止时间（与 K8s terminationGracePeriod 对齐）。
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// §6.1 / §12：运行全部 server future，按首个结束者的语义决定全局关闭。
///
/// - `Err(_)` 或（`Ok(())` 且 shutdown 未请求）→ `shutdown_tx.send(true)` 广播关闭，
///   `DRAIN_TIMEOUT` 内排空其余任务，返回 `Err(anyhow!("服务 {name} 意外退出"))`；
/// - shutdown 已请求（信号触发的正常路径）→ 排空其余任务，返回 `Ok(())`；
/// - 排空超时仅 `warn!` 后返回（不阻塞进程退出）。
pub async fn run_servers(
    servers: Vec<(String, ServerFuture)>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut set: JoinSet<(String, anyhow::Result<()>)> = JoinSet::new();
    for (name, fut) in servers {
        set.spawn(async move { (name, fut.await) });
    }

    // 首个结束的任务决定后续语义（正常情况下由信号广播使既有 serve 依次优雅结束）。
    if let Some(joined) = set.join_next().await {
        match joined {
            // 正常路径：关闭已被请求，一个服务优雅退出。
            Ok((name, Ok(()))) if *shutdown_rx.borrow() => {
                tracing::info!(server = %name, "服务已优雅退出");
            }
            // 异常：未请求关闭即自行结束。
            Ok((name, Ok(()))) => {
                tracing::error!(server = %name, "服务未请求关闭即退出，触发全局关闭");
                return fail_and_drain(name, &shutdown_tx, &mut set).await;
            }
            // 异常：listener 返回错误。
            Ok((name, Err(e))) => {
                tracing::error!(server = %name, error = %e, "服务异常退出，触发全局关闭");
                return fail_and_drain(name, &shutdown_tx, &mut set).await;
            }
            // 异常：任务 panic 或被取消。
            Err(e) => {
                tracing::error!(error = %e, "服务任务异常退出，触发全局关闭");
                let _ = shutdown_tx.send(true);
                drain(&mut set).await;
                return Err(anyhow::anyhow!("服务任务异常退出: {e}"));
            }
        }
    }

    drain(&mut set).await;
    Ok(())
}

/// 异常退出：广播全局关闭，排空其余任务后返回错误。
async fn fail_and_drain(
    name: String,
    shutdown_tx: &watch::Sender<bool>,
    set: &mut JoinSet<(String, anyhow::Result<()>)>,
) -> anyhow::Result<()> {
    let _ = shutdown_tx.send(true);
    drain(set).await;
    Err(anyhow::anyhow!("服务 {name} 意外退出"))
}

/// 排空 `set` 中剩余任务；超时仅 `warn!`（不阻塞进程退出）。
async fn drain(set: &mut JoinSet<(String, anyhow::Result<()>)>) {
    let drained = async { while set.join_next().await.is_some() {} };
    if tokio::time::timeout(DRAIN_TIMEOUT, drained).await.is_err() {
        tracing::warn!(
            timeout_secs = DRAIN_TIMEOUT.as_secs(),
            "等待在途请求排空超时，强制退出"
        );
    }
}

/// 将 watch 通道转换为 `axum::serve(...).with_graceful_shutdown` 所需的 future。
///
/// 先检查当前值（关闭已广播时立即返回），否则等待下一次变更。
pub async fn watch_shutdown(mut rx: watch::Receiver<bool>) {
    if *rx.borrow() {
        return;
    }
    let _ = rx.changed().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn listener_failure_triggers_global_shutdown() {
        let (tx, rx) = watch::channel(false);
        let mut waiter = rx.clone();
        let servers = vec![
            (
                "bad".into(),
                Box::pin(async { Err(anyhow::anyhow!("boom")) }) as ServerFuture,
            ),
            (
                "wait".into(),
                Box::pin(async move {
                    while !*waiter.borrow() {
                        if waiter.changed().await.is_err() {
                            break;
                        }
                    }
                    Ok(())
                }) as ServerFuture,
            ),
        ];
        let err = run_servers(servers, tx, rx).await.unwrap_err();
        assert!(err.to_string().contains("bad"), "{err}");
    }

    #[tokio::test]
    async fn graceful_shutdown_returns_ok() {
        fn waiter(mut rx: watch::Receiver<bool>) -> ServerFuture {
            Box::pin(async move {
                while !*rx.borrow() {
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
                Ok(())
            })
        }

        let (tx, rx) = watch::channel(false);
        // 关闭先于 run_servers 广播：两个 server future 立即观察到并优雅返回。
        tx.send(true).expect("广播关闭失败");
        let servers = vec![
            ("a".to_string(), waiter(rx.clone())),
            ("b".to_string(), waiter(rx.clone())),
        ];
        run_servers(servers, tx, rx)
            .await
            .expect("优雅关闭应返回 Ok");
    }
}
