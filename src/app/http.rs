use anyhow::Context;

const USER_AGENT: &str = "codex-switch/0.1.0";

pub fn build_client(proxy_url: Option<&str>) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().user_agent(USER_AGENT);
    if let Some(proxy_url) = proxy_url.map(str::trim).filter(|value| !value.is_empty()) {
        builder = builder.proxy(proxy_from_url(proxy_url)?);
    }
    builder.build().context("failed to build http client")
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
