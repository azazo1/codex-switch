use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

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
}

#[derive(Debug, Clone)]
pub struct LiveRequestSnapshot {
    pub id: String,
    pub upstream_name: Option<String>,
    pub endpoint: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub tail: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct LiveRequest {
    meta: LiveRequestMeta,
    tail: String,
    started_at: DateTime<Utc>,
}

impl LiveRequestStore {
    pub fn start(&self, meta: LiveRequestMeta) {
        let request = LiveRequest {
            meta: meta.clone(),
            tail: String::new(),
            started_at: Utc::now(),
        };
        if let Ok(mut inner) = self.inner.write() {
            inner.insert(meta.id, request);
        }
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

    pub fn finish(&self, id: &str) {
        if let Ok(mut inner) = self.inner.write() {
            inner.remove(id);
        }
    }

    pub fn snapshots(&self) -> Vec<LiveRequestSnapshot> {
        let Ok(inner) = self.inner.read() else {
            return Vec::new();
        };
        inner
            .values()
            .map(|request| LiveRequestSnapshot {
                id: request.meta.id.clone(),
                upstream_name: request.meta.upstream_name.clone(),
                endpoint: request.meta.endpoint.clone(),
                model: request.meta.model.clone(),
                reasoning_effort: request.meta.reasoning_effort.clone(),
                tail: request.tail.clone(),
                started_at: request.started_at,
            })
            .collect()
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
