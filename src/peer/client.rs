use anyhow::Context;
use reqwest::Client;

use super::cert::{pinned_client_config, tofu_client_config};
use super::identity::NodeIdentity;
use super::protocol::{PairRequest, PairResponse, PairStatus, PeerIdentityPayload};

const USER_AGENT: &str = concat!("codex-switch-peer/", env!("CARGO_PKG_VERSION"));

pub fn tofu_client(identity: &NodeIdentity) -> anyhow::Result<Client> {
    build_client(tofu_client_config(identity)?)
}

pub fn pinned_client(identity: &NodeIdentity, public_key: [u8; 32]) -> anyhow::Result<Client> {
    build_client(pinned_client_config(identity, public_key)?)
}

fn build_client(tls: rustls::ClientConfig) -> anyhow::Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .use_preconfigured_tls(tls)
        .https_only(true)
        .build()
        .context("failed to build peer http client")
}

pub async fn send_pair_request(
    client: &Client,
    base_url: &str,
    identity: PeerIdentityPayload,
) -> anyhow::Result<PairResponse> {
    let url = format!("{}/peer/v1/pair", base_url.trim_end_matches('/'));
    let response = client
        .post(url)
        .json(&PairRequest { identity })
        .send()
        .await
        .context("failed to send peer pair request")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read peer pair response")?;
    if !status.is_success() {
        anyhow::bail!("peer pair request failed: {status} {body}");
    }
    serde_json::from_str(&body).context("invalid peer pair response")
}

pub async fn poll_pair_until_resolved(
    client: &Client,
    base_url: &str,
    identity: PeerIdentityPayload,
) -> anyhow::Result<PeerIdentityPayload> {
    for _ in 0..40 {
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
                tracing::info!("waiting for peer pairing confirmation");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
    anyhow::bail!("timed out waiting for peer pairing confirmation")
}


