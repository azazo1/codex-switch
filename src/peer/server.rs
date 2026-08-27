use crate::app::AppState;
use crate::peer::cert::{server_config, tls_identity_from_certs};
use crate::peer::protocol::{
    PairRequest, PairResponse, PairStatus, PeerIdentityPayload, PeerTlsIdentity,
};
use crate::proxy::forward::auth::{PEER_FINGERPRINT_HEADER, PEER_PUBLIC_KEY_HEADER};
use crate::proxy::router;
use anyhow::Context;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::ServerConfig;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_rustls::TlsAcceptor;
use tower::Service;

pub struct PeerServerHandle {
    shutdown: Option<oneshot::Sender<()>>,
}

impl PeerServerHandle {
    pub fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

#[derive(Clone)]
struct PeerHttpState {
    app: AppState,
}

pub async fn start_peer_server(
    bind_addr: String,
    state: AppState,
) -> anyhow::Result<PeerServerHandle> {
    let addr: SocketAddr = bind_addr
        .parse()
        .with_context(|| format!("invalid peer bind address {bind_addr}"))?;
    let identity = state.peers.identity();
    let tls = Arc::new(server_config(&identity)?);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind peer address {addr}"))?;
    let app = build_peer_router(state);
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        tracing::info!(%addr, "peer tls server started");
        tokio::select! {
            _ = accept_loop(listener, tls, app) => {}
            _ = rx => {}
        }
        tracing::info!("peer tls server stopped");
    });
    Ok(PeerServerHandle { shutdown: Some(tx) })
}

fn build_peer_router(state: AppState) -> Router {
    let peer_state = PeerHttpState { app: state.clone() };
    let pair = Router::new()
        .route("/peer/v1/pair", post(pair_handler))
        .with_state(peer_state);
    Router::new()
        .merge(pair)
        .merge(router::build_api_router(state))
}

async fn accept_loop(listener: tokio::net::TcpListener, tls: Arc<ServerConfig>, app: Router) {
    let acceptor = TlsAcceptor::from(tls);
    loop {
        let accepted = listener.accept().await;
        let Ok((stream, addr)) = accepted else {
            tracing::warn!("peer tls accept failed");
            continue;
        };
        let acceptor = acceptor.clone();
        let app = app.clone();
        tokio::spawn(async move {
            if let Err(err) = serve_peer_connection(acceptor, stream, app).await {
                tracing::debug!(%addr, error = %err, "peer tls connection ended");
            }
        });
    }
}

async fn serve_peer_connection(
    acceptor: TlsAcceptor,
    stream: tokio::net::TcpStream,
    app: Router,
) -> anyhow::Result<()> {
    let tls_stream = acceptor
        .accept(stream)
        .await
        .context("peer tls handshake failed")?;
    let peer_identity = tls_stream
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certs| tls_identity_from_certs(certs).ok());
    let service = hyper::service::service_fn(move |mut request: axum::http::Request<Incoming>| {
        let mut app = app.clone();
        if let Some(identity) = peer_identity.clone() {
            request.extensions_mut().insert(identity.clone());
            if let Ok(value) = axum::http::HeaderValue::try_from(crate::peer::identity::base64_public_key(&identity.public_key)) {
                request.headers_mut().insert(PEER_PUBLIC_KEY_HEADER, value);
            }
            if let Ok(value) = axum::http::HeaderValue::try_from(identity.fingerprint.clone()) {
                request.headers_mut().insert(PEER_FINGERPRINT_HEADER, value);
            }
        }
        async move { app.call(request).await }
    });
    hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(tls_stream), service)
        .await
        .map_err(|err| anyhow::anyhow!("peer http connection failed: {err}"))?;
    Ok(())
}

async fn pair_handler(
    State(state): State<PeerHttpState>,
    Extension(tls): Extension<PeerTlsIdentity>,
    Json(request): Json<PairRequest>,
) -> Response {
    match handle_pair_request(&state.app, &tls, request.identity).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(err) => {
            tracing::warn!(error = %err, "peer pair request failed");
            (
                StatusCode::BAD_REQUEST,
                Json(PairResponse {
                    status: PairStatus::Rejected,
                    request_id: None,
                    identity: None,
                    message: Some(err.to_string()),
                }),
            )
                .into_response()
        }
    }
}

async fn handle_pair_request(
    state: &AppState,
    tls: &PeerTlsIdentity,
    payload: PeerIdentityPayload,
) -> anyhow::Result<PairResponse> {
    let public_key = payload.verify()?;
    if public_key != tls.public_key {
        anyhow::bail!("pairing payload public key does not match tls certificate");
    }
    if payload.node_id == state.peers.identity().node_id {
        anyhow::bail!("refusing to pair with local node");
    }
    if let Some(existing) = state.store.get_node_peer(&payload.node_id).await? {
        if existing.public_key != payload.public_key {
            anyhow::bail!("peer public key does not match existing pairing");
        }
        let local = local_identity_payload(state).await?;
        return Ok(PairResponse {
            status: PairStatus::Accepted,
            request_id: None,
            identity: Some(local),
            message: None,
        });
    }
    if let Some(pending) = state
        .store
        .get_peer_pairing_request_by_node(&payload.node_id)
        .await?
    {
        return Ok(PairResponse {
            status: PairStatus::Pending,
            request_id: Some(pending.id),
            identity: None,
            message: Some("waiting for local confirmation".to_string()),
        });
    }
    let pending = crate::storage::Store::new_pairing_request(&payload);
    state.store.save_peer_pairing_request(&pending).await?;
    state.events.bump_peers();
    tracing::info!(
        node_id = %payload.node_id,
        fingerprint = %payload.fingerprint,
        "received inbound peer pairing request"
    );
    Ok(PairResponse {
        status: PairStatus::Pending,
        request_id: Some(pending.id),
        identity: None,
        message: Some("waiting for local confirmation".to_string()),
    })
}

pub async fn local_identity_payload(state: &AppState) -> anyhow::Result<PeerIdentityPayload> {
    let identity = state.peers.identity();
    let bind_addr = state.store.peer_bind_addr().await?;
    let port = bind_addr
        .parse::<SocketAddr>()
        .map(|addr| addr.port())
        .unwrap_or(15722);
    Ok(PeerIdentityPayload::from_identity(
        &identity,
        crate::peer::discovery::local_peer_addresses(port),
    ))
}


