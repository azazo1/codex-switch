use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use tokio::sync::watch;

const MAX_TAIL_BYTES: usize = 768;

#[derive(Clone, Default)]
pub struct LiveRequestStore {
    inner: Arc<RwLock<BTreeMap<String, LiveRequest>>>,
}

#[derive(Debug, Clone)]
pub struct LiveRequestMeta {
    pub id: String,
    pub upstream_name: Option<String>,
    pub endpoint: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub streaming: bool,
}

#[derive(Debug, Clone)]
pub struct LiveRequestSnapshot {
    pub id: String,
    pub upstream_name: Option<String>,
    pub endpoint: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub streaming: bool,
    pub tail: String,
    pub started_at: DateTime<Utc>,
    pub terminating: bool,
}

#[derive(Debug, Clone)]
struct LiveRequest {
    meta: LiveRequestMeta,
    tail: String,
    started_at: DateTime<Utc>,
    terminate_tx: watch::Sender<bool>,
    terminating: bool,
}

impl LiveRequestStore {
    pub fn start(&self, meta: LiveRequestMeta) -> watch::Receiver<bool> {
        let (terminate_tx, terminate_rx) = watch::channel(false);
        let request = LiveRequest {
            meta: meta.clone(),
            tail: String::new(),
            started_at: Utc::now(),
            terminate_tx,
            terminating: false,
        };
        if let Ok(mut inner) = self.inner.write() {
            inner.insert(meta.id, request);
        }
        terminate_rx
    }

    pub fn append_delta(&self, id: &str, delta: &str) -> bool {
        if delta.is_empty() {
            return false;
        }
        let Ok(mut inner) = self.inner.write() else {
            return false;
        };
        let Some(request) = inner.get_mut(id) else {
            return false;
        };
        append_tail(&mut request.tail, delta);
        true
    }

    pub fn set_streaming(&self, id: &str, streaming: bool) -> bool {
        let Ok(mut inner) = self.inner.write() else {
            return false;
        };
        let Some(request) = inner.get_mut(id) else {
            return false;
        };
        request.meta.streaming = streaming;
        true
    }

    pub fn finish(&self, id: &str) {
        if let Ok(mut inner) = self.inner.write() {
            inner.remove(id);
        }
    }

    pub fn terminate(&self, id: &str) -> bool {
        let Ok(mut inner) = self.inner.write() else {
            return false;
        };
        let Some(request) = inner.get_mut(id) else {
            return false;
        };
        request.terminating = true;
        request.terminate_tx.send_replace(true);
        true
    }

    pub fn snapshots(&self) -> Vec<LiveRequestSnapshot> {
        let Ok(inner) = self.inner.read() else {
            return Vec::new();
        };
        let mut snapshots = inner
            .values()
            .map(|request| LiveRequestSnapshot {
                id: request.meta.id.clone(),
                upstream_name: request.meta.upstream_name.clone(),
                endpoint: request.meta.endpoint.clone(),
                model: request.meta.model.clone(),
                reasoning_effort: request.meta.reasoning_effort.clone(),
                streaming: request.meta.streaming,
                tail: request.tail.clone(),
                started_at: request.started_at,
                terminating: request.terminating,
            })
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        snapshots
    }
}

fn append_tail(tail: &mut String, delta: &str) {
    for ch in delta.chars() {
        if ch == '\n' || ch == '\r' {
            tail.push(' ');
        } else {
            tail.push(ch);
        }
    }
    if tail.len() <= MAX_TAIL_BYTES {
        return;
    }
    let keep_from = tail.len() - MAX_TAIL_BYTES;
    let split = tail
        .char_indices()
        .find(|(index, _)| *index >= keep_from)
        .map(|(index, _)| index)
        .unwrap_or(0);
    if split > 0 {
        tail.drain(..split);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_meta() -> LiveRequestMeta {
        LiveRequestMeta {
            id: "request-a".to_string(),
            upstream_name: Some("upstream-a".to_string()),
            endpoint: "/responses".to_string(),
            model: Some("model-a".to_string()),
            reasoning_effort: None,
            streaming: true,
        }
    }

    #[tokio::test]
    async fn terminate_marks_snapshot_and_notifies_receiver() {
        let store = LiveRequestStore::default();
        let mut terminate_rx = store.start(request_meta());

        assert!(!store.snapshots()[0].terminating);
        assert!(store.terminate("request-a"));
        assert!(store.snapshots()[0].terminating);

        terminate_rx.changed().await.unwrap();
        assert!(*terminate_rx.borrow());
    }

    #[test]
    fn finish_removes_request() {
        let store = LiveRequestStore::default();
        store.start(request_meta());

        store.finish("request-a");

        assert!(store.snapshots().is_empty());
    }

    #[test]
    fn set_streaming_updates_snapshot_kind() {
        let store = LiveRequestStore::default();
        let mut meta = request_meta();
        meta.streaming = false;
        store.start(meta);

        assert!(!store.snapshots()[0].streaming);
        assert!(store.set_streaming("request-a", true));

        assert!(store.snapshots()[0].streaming);
    }

    #[test]
    fn snapshots_are_sorted_by_start_time() {
        let store = LiveRequestStore::default();
        store.start(request_meta());
        let mut meta = request_meta();
        meta.id = "request-b".to_string();
        store.start(meta);

        let snapshots = store.snapshots();

        assert!(snapshots[0].started_at <= snapshots[1].started_at);
    }
}
