//! 单实例锁.
//!
//! 锁与数据目录绑定: 持锁实例在数据目录对应的本地 endpoint 上监听
//! (unix domain socket 或 Windows named pipe), 二次启动的进程连接成功后
//! 转发启动参数并退出, 持锁实例收到通知后显示并聚焦主窗口.

use anyhow::Context;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_MESSAGE_LEN: usize = 4096;

/// acquire 的结果.
pub(crate) enum AcquireOutcome {
    /// 本进程持锁, 需要调用 [`InstanceListener::serve`] 接受二次启动通知.
    Primary(InstanceListener),
    /// 已有实例存活且已收到通知, 本进程应当直接退出.
    Duplicate,
}

/// 尝试成为数据目录的唯一实例.
pub(crate) async fn acquire(data_dir: &Path) -> anyhow::Result<AcquireOutcome> {
    #[cfg(unix)]
    {
        let socket_path = data_dir.join("codex-switch.sock");
        if notify_existing_unix(&socket_path).await {
            return Ok(AcquireOutcome::Duplicate);
        }
        // 没有监听者的 socket 属于上次异常退出留下的陈旧文件, 清理后重建.
        let _ = std::fs::remove_file(&socket_path);
        let listener = tokio::net::UnixListener::bind(&socket_path).with_context(|| {
            format!(
                "failed to bind single instance socket {}",
                socket_path.display()
            )
        })?;
        tracing::info!(path = %socket_path.display(), "single instance lock acquired");
        return Ok(AcquireOutcome::Primary(InstanceListener {
            endpoint: Mutex::new(Some(Endpoint::Unix {
                listener,
                _socket_file: UnixSocketFile { path: socket_path },
            })),
            shutdown_tx: Mutex::new(None),
        }));
    }
    #[cfg(windows)]
    {
        let pipe_name = pipe_name_for(data_dir);
        return match tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)
        {
            Ok(server) => {
                tracing::info!(pipe = %pipe_name, "single instance lock acquired");
                Ok(AcquireOutcome::Primary(InstanceListener {
                    endpoint: Mutex::new(Some(Endpoint::Windows { server, pipe_name })),
                    shutdown_tx: Mutex::new(None),
                }))
            }
            // ERROR_ACCESS_DENIED: named pipe 已存在, 说明已有实例持锁.
            Err(err) if err.raw_os_error() == Some(5) => {
                if notify_existing_windows(&pipe_name).await {
                    Ok(AcquireOutcome::Duplicate)
                } else {
                    anyhow::bail!("single instance pipe {pipe_name} exists but is not answering");
                }
            }
            Err(err) => Err(err)
                .with_context(|| format!("failed to create single instance pipe {pipe_name}")),
        };
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = data_dir;
        anyhow::bail!("single instance lock is not supported on this platform");
    }
}

/// 持锁端: serve 后接受二次启动通知, release 或 drop 时释放锁.
pub(crate) struct InstanceListener {
    endpoint: Mutex<Option<Endpoint>>,
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
}

impl Drop for InstanceListener {
    fn drop(&mut self) {
        self.release();
    }
}

impl InstanceListener {
    /// 启动接受循环; 每个二次启动连接都会触发一次 `on_message`, 只生效一次.
    pub(crate) fn serve(
        self: &Arc<Self>,
        runtime: tokio::runtime::Handle,
        on_message: Arc<dyn Fn(String) + Send + Sync>,
    ) {
        let endpoint = self
            .endpoint
            .lock()
            .expect("single instance endpoint poisoned")
            .take();
        let Some(endpoint) = endpoint else {
            return;
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        if let Ok(mut guard) = self.shutdown_tx.lock() {
            *guard = Some(shutdown_tx);
        }
        runtime.spawn(accept_loop(endpoint, shutdown_rx, on_message));
        tracing::info!("single instance listener started");
    }

    /// 释放锁并停止接受循环; 更新安装重启前必须先调用, 避免新进程抢锁失败.
    pub(crate) fn release(&self) {
        if let Ok(mut guard) = self.shutdown_tx.lock()
            && let Some(shutdown_tx) = guard.take()
        {
            let _ = shutdown_tx.send(());
        }
    }
}

enum Endpoint {
    #[cfg(unix)]
    Unix {
        listener: tokio::net::UnixListener,
        _socket_file: UnixSocketFile,
    },
    #[cfg(windows)]
    Windows {
        server: tokio::net::windows::named_pipe::NamedPipeServer,
        pipe_name: String,
    },
}

#[cfg(unix)]
struct UnixSocketFile {
    path: PathBuf,
}

#[cfg(unix)]
impl Drop for UnixSocketFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn accept_loop(
    endpoint: Endpoint,
    mut shutdown_rx: oneshot::Receiver<()>,
    on_message: Arc<dyn Fn(String) + Send + Sync>,
) {
    match endpoint {
        #[cfg(unix)]
        Endpoint::Unix {
            listener,
            _socket_file,
        } => {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => match accepted {
                        Ok((mut stream, _)) => {
                            handle_connection(&mut stream, &on_message).await;
                        }
                        Err(err) => {
                            tracing::debug!(error = %err, "single instance listener closed");
                            break;
                        }
                    },
                }
            }
        }
        #[cfg(windows)]
        Endpoint::Windows { mut server, pipe_name } => {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    connected = server.connect() => {
                        if let Err(err) = connected {
                            tracing::debug!(error = %err, "single instance pipe closed");
                            break;
                        }
                        let mut client = server;
                        server = match tokio::net::windows::named_pipe::ServerOptions::new()
                            .create(&pipe_name)
                        {
                            Ok(server) => server,
                            Err(err) => {
                                tracing::warn!(error = %err, "failed to recreate single instance pipe");
                                break;
                            }
                        };
                        handle_connection(&mut client, &on_message).await;
                    },
                }
            }
        }
    }
    tracing::info!("single instance listener stopped");
}

async fn handle_connection<S>(
    stream: &mut S,
    on_message: &Arc<dyn Fn(String) + Send + Sync>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match tokio::time::timeout(CONNECT_TIMEOUT, read_message(stream)).await {
        Ok(Ok(message)) => {
            if message.is_empty() {
                tracing::info!("second instance notification received");
            } else {
                tracing::info!(arguments = %message, "second instance notification received");
            }
            on_message(message);
        }
        Ok(Err(err)) => {
            tracing::debug!(error = %err, "failed to read second instance message");
        }
        Err(_) => {
            tracing::debug!("second instance message read timed out");
        }
    }
}

async fn read_message(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> std::io::Result<String> {
    let mut buffer = vec![0u8; MAX_MESSAGE_LEN];
    let mut used = 0;
    loop {
        let read = stream.read(&mut buffer[used..]).await?;
        used += read;
        if read == 0 || used == buffer.len() || buffer[..used].contains(&b'\n') {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buffer[..used])
        .trim_end()
        .to_string())
}

#[cfg(unix)]
async fn notify_existing_unix(socket_path: &Path) -> bool {
    let connect = tokio::net::UnixStream::connect(socket_path);
    let mut stream = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(_)) | Err(_) => return false,
    };
    let arguments = launch_arguments();
    if let Err(err) = stream.write_all(arguments.as_bytes()).await {
        tracing::warn!(error = %err, "failed to forward launch arguments");
    }
    let _ = stream.shutdown().await;
    true
}

#[cfg(windows)]
async fn notify_existing_windows(pipe_name: &str) -> bool {
    let Ok(mut client) = tokio::net::windows::named_pipe::ClientOptions::new().open(pipe_name)
    else {
        return false;
    };
    let arguments = launch_arguments();
    if let Err(err) = client.write_all(arguments.as_bytes()).await {
        tracing::warn!(error = %err, "failed to forward launch arguments");
    }
    let _ = client.flush().await;
    let _ = client.shutdown().await;
    true
}

/// 二次启动转发的启动参数, 当前仅用于日志与审计.
fn launch_arguments() -> String {
    let mut line = std::env::args()
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ");
    line.push('\n');
    line
}

#[cfg(windows)]
fn pipe_name_for(data_dir: &Path) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(data_dir.to_string_lossy().as_bytes());
    let hash: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!(r"\\.\pipe\codex-switch-{hash}")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acquire_binds_socket_and_releases_on_drop() {
        let dir = std::env::temp_dir().join(format!(
            "codex-switch-single-instance-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let socket_path = dir.join("codex-switch.sock");
        let _ = std::fs::remove_file(&socket_path);

        let listener = match acquire(&dir).await.unwrap() {
            AcquireOutcome::Primary(listener) => listener,
            AcquireOutcome::Duplicate => panic!("unexpected duplicate"),
        };
        assert!(socket_path.exists());
        drop(listener);
        assert!(!socket_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
