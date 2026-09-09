use crate::logging::network::HttpClient;
use anyhow::Context;

fn user_agent() -> String {
    format!("codex-switch/{}", super::build_info::display_version())
}

pub fn build_client(proxy_url: Option<&str>) -> anyhow::Result<HttpClient> {
    let mut builder = reqwest::Client::builder().user_agent(user_agent());
    if let Some(proxy_url) = proxy_url.map(str::trim).filter(|value| !value.is_empty()) {
        builder = builder.proxy(proxy_from_url(proxy_url)?);
    }
    let client = builder.build().context("failed to build http client")?;
    Ok(HttpClient::from_client(client))
}

pub fn validate_proxy_url(proxy_url: &str) -> anyhow::Result<()> {
    if proxy_url.trim().is_empty() {
        return Ok(());
    }
    proxy_from_url(proxy_url.trim()).map(|_| ())
}

fn proxy_from_url(proxy_url: &str) -> anyhow::Result<reqwest::Proxy> {
    reqwest::Proxy::all(proxy_url)
        .with_context(|| format!("invalid upstream proxy URL: {proxy_url}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_follows_build_version() {
        assert_eq!(
            user_agent(),
            format!("codex-switch/{}", super::super::build_info::display_version())
        );
    }
}
