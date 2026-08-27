use anyhow::Context;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use std::io;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use super::cert::{pinned_client_config, tofu_client_config};
use super::identity::NodeIdentity;
use super::protocol::{PairRequest, PairResponse, PairStatus, PeerIdentityPayload};

const USER_AGENT: &str = concat!("codex-switch-peer/", env!("CARGO_PKG_VERSION"));

#[derive(Clone)]
pub struct PeerHttpClient {
    tls: Arc<rustls::ClientConfig>,
}

pub struct PeerHttpResponse {
    pub status: hyper::StatusCode,
    pub headers: hyper::HeaderMap,
    body: Incoming,
}

impl PeerHttpClient {
    pub fn tofu(identity: &NodeIdentity) -> anyhow::Result<Self> {
        Ok(Self {
            tls: Arc::new(tofu_client_config(identity)?),
        })
    }

    pub fn pinned(identity: &NodeIdentity, public_key: [u8; 32]) -> anyhow::Result<Self> {
        Ok(Self {
            tls: Arc::new(pinned_client_config(identity, public_key)?),
        })
    }

    pub async fn send(
        &self,
        method: &str,
        url: &str,
        headers: hyper::HeaderMap,
        body: Vec<u8>,
    ) -> anyhow::Result<PeerHttpResponse> {
        let parsed = url::Url::parse(url).context("invalid peer url")?;
        let host = parsed
            .host_str()
            .context("peer url is missing host")?
            .to_string();
        let port = parsed.port().unwrap_or(15722);
        let path = if let Some(query) = parsed.query() {
            format!("{}?{query}", parsed.path())
        } else if parsed.path().is_empty() {
            "/".to_string()
        } else {
            parsed.path().to_string()
        };
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", format!("{host}:{port}"))
            .header("user-agent", USER_AGENT);
        for (name, value) in &headers {
            request = request.header(name, value);
        }
        let request = request
            .body(Full::new(Bytes::from(body)))
            .context("failed to build peer request")?;
        send_https_request(self.tls.clone(), &host, port, request).await
    }
}

impl PeerHttpResponse {
    pub async fn bytes(self) -> anyhow::Result<Bytes> {
        Ok(self
            .body
            .collect()
            .await
            .context("failed to read peer response")?
            .to_bytes())
    }

    pub fn bytes_stream(self) -> impl Stream<Item = Result<Bytes, io::Error>> {
        self.body
            .into_data_stream()
            .map(|item| item.map_err(io::Error::other))
    }
}

pub fn tofu_client(identity: &NodeIdentity) -> anyhow::Result<PeerHttpClient> {
    PeerHttpClient::tofu(identity)
}

pub fn pinned_client(
    identity: &NodeIdentity,
    public_key: [u8; 32],
) -> anyhow::Result<PeerHttpClient> {
    PeerHttpClient::pinned(identity, public_key)
}

pub async fn send_pair_request(
    client: &PeerHttpClient,
    base_url: &str,
    identity: PeerIdentityPayload,
) -> anyhow::Result<PairResponse> {
    let url = format!("{}/peer/v1/pair", base_url.trim_end_matches('/'));
    let body = serde_json::to_vec(&PairRequest { identity }).context("failed to encode pair request")?;
    let mut headers = hyper::HeaderMap::new();
    headers.insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("application/json"),
    );
    tracing::info!(url = %url, "sending peer pair request");
    let response = client.send("POST", &url, headers, body).await?;
    let status = response.status;
    let body = response.bytes().await?;
    if !status.is_success() {
        anyhow::bail!(
            "peer pair request failed: {} {}",
            status,
            String::from_utf8_lossy(&body)
        );
    }
    serde_json::from_slice(&body).context("invalid peer pair response")
}

pub async fn poll_pair_until_resolved(
    client: &PeerHttpClient,
    base_url: &str,
    identity: PeerIdentityPayload,
) -> anyhow::Result<PeerIdentityPayload> {
    for attempt in 1..=40 {
        let response = send_pair_request(client, base_url, identity.clone()).await?;
        match response.status {
            PairStatus::Accepted => {
                let remote = response
                    .identity
                    .ok_or_else(|| anyhow::anyhow!("accepted pair response is missing identity"))?;
                remote.verify()?;
                return Ok(remote);
            }
            PairStatus::Rejected => {
                anyhow::bail!(
                    "{}",
                    response
                        .message
                        .unwrap_or_else(|| "peer rejected pairing".to_string())
                );
            }
            PairStatus::Pending => {
                tracing::info!(
                    attempt,
                    "waiting for peer pairing confirmation"
                );
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
    anyhow::bail!("timed out waiting for peer pairing confirmation")
}

async fn send_https_request(
    tls: Arc<rustls::ClientConfig>,
    host: &str,
    port: u16,
    request: Request<Full<Bytes>>,
) -> anyhow::Result<PeerHttpResponse> {
    let stream = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("failed to connect to peer {host}:{port}"))?;
    let connector = TlsConnector::from(tls);
    let server_name = ServerName::try_from("codex-switch-peer")
        .context("invalid peer tls server name")?
        .to_owned();
    let tls_stream = connector
        .connect(server_name, stream)
        .await
        .context("peer tls handshake failed")?;
    let io = TokioIo::new(tls_stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .context("peer http handshake failed")?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let response = sender
        .send_request(request)
        .await
        .context("failed to send peer request")?;
    Ok(PeerHttpResponse {
        status: response.status(),
        headers: response.headers().clone(),
        body: response.into_body(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;
    use crate::cache_keepalive::CacheKeepaliveRuntime;
    use crate::peer::protocol::PairStatus;
    use crate::peer::server::start_peer_server;
    use crate::storage::{Store, credentials::CredentialStore};

    async fn test_state() -> AppState {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-peer-probe-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(path).await.unwrap();
        let credentials = CredentialStore::new_for_tests(store.clone());
        let events: crate::app::AppEvents = Default::default();
        let cache_keepalive = CacheKeepaliveRuntime::new(
            store.clone(),
            credentials.clone(),
            reqwest::Client::new(),
            events.clone(),
        );
        let oauth_accounts = crate::oauth::OAuthAccountService::new(store.clone());
        let peers = crate::peer::PeerRuntime::new(&store).await.unwrap();
        AppState {
            store,
            model_capabilities: Default::default(),
            credentials,
            oauth_accounts,
            http: reqwest::Client::new(),
            events,
            scheduler: Default::default(),
            live_requests: Default::default(),
            cache_keepalive,
            peers,
        }
    }

    async fn bind_local() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().to_string()
    }

    #[tokio::test]
    async fn pair_request_reaches_local_peer_listener() {
        let state = test_state().await;
        let addr = bind_local().await;
        let handle = start_peer_server(addr.clone(), state.clone()).await.unwrap();
        let identity = crate::peer::identity::NodeIdentity::generate("probe".to_string()).unwrap();
        let client = tofu_client(&identity).unwrap();
        let payload = PeerIdentityPayload::from_identity(&identity, vec![format!("https://{addr}")]);
        let response = send_pair_request(&client, &format!("https://{addr}"), payload)
            .await
            .unwrap();
        handle.stop();
        assert_eq!(response.status, PairStatus::Pending);
        assert!(response.request_id.is_some());
    }

    #[tokio::test]
    async fn local_nodes_can_complete_pairing() {
        let local = test_state().await;
        let remote = test_state().await;
        let local_addr = bind_local().await;
        let remote_addr = bind_local().await;
        let local_handle = start_peer_server(local_addr.clone(), local.clone())
            .await
            .unwrap();
        let remote_handle = start_peer_server(remote_addr.clone(), remote.clone())
            .await
            .unwrap();
        let client = tofu_client(&local.peers.identity()).unwrap();
        let payload = PeerIdentityPayload::from_identity(
            &local.peers.identity(),
            vec![format!("https://{local_addr}")],
        );
        let pending = send_pair_request(&client, &format!("https://{remote_addr}"), payload.clone())
            .await
            .unwrap();
        assert_eq!(pending.status, PairStatus::Pending);
        let requests = remote
            .store
            .list_peer_pairing_requests()
            .await
            .unwrap();
        assert_eq!(requests.len(), 1);
        remote
            .peers
            .accept_pairing_request(&remote.store, &requests[0].id)
            .await
            .unwrap();
        let accepted =
            send_pair_request(&client, &format!("https://{remote_addr}"), payload)
                .await
                .unwrap();
        local_handle.stop();
        remote_handle.stop();
        assert_eq!(accepted.status, PairStatus::Accepted);
        assert_eq!(
            accepted.identity.unwrap().fingerprint,
            remote.peers.identity().fingerprint()
        );
    }
}
