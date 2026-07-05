use crate::app::AppState;
use crate::proxy::router;
use anyhow::Context;
use tokio::sync::oneshot;

pub struct ServerHandle {
    shutdown: Option<oneshot::Sender<()>>,
}

impl ServerHandle {
    pub fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

pub async fn start_server(bind_addr: String, state: AppState) -> anyhow::Result<ServerHandle> {
    let addr = bind_addr
        .parse::<std::net::SocketAddr>()
        .with_context(|| format!("invalid bind address {bind_addr}"))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    let app = router::build_router(state);
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        tracing::info!(%addr, "proxy server started");
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
        if let Err(err) = result {
            tracing::error!(error = %err, "proxy server stopped with error");
        } else {
            tracing::info!("proxy server stopped");
        }
    });
    Ok(ServerHandle { shutdown: Some(tx) })
}
