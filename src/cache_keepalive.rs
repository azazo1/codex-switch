mod body;
mod key;
mod runtime;
mod session;

#[cfg(test)]
mod tests;

use std::time::Duration;

pub use runtime::CacheKeepaliveRuntime;
pub use session::{CacheKeepaliveRegistration, CacheKeepaliveSessionSnapshot};

const SCAN_INTERVAL: Duration = Duration::from_secs(15);
const KEEPALIVE_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const DISABLED_SESSION_RETENTION: Duration = Duration::from_secs(5);
const INTERNAL_ENDPOINT: &str = "/internal/cache_keepalive";
const OUTPUT_TOKENS_WARNING_THRESHOLD: i64 = 8;
