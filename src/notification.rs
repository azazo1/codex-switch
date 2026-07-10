use anyhow::Context;
use notify_rust::Notification;

pub async fn send(title: String, body: String) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || send_blocking(&title, &body))
        .await
        .context("system notification task failed")?
}

fn send_blocking(title: &str, body: &str) -> anyhow::Result<()> {
    Notification::new()
        .appname("Codex Switch")
        .summary(title)
        .body(body)
        .show()
        .context("failed to show system notification")?;
    Ok(())
}
