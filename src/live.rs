use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use tokio::sync::watch;

const MAX_TAIL_BYTES: usize = 768;
const MAX_HOVER_OUTPUT_LINES: usize = 10;
const MAX_HOVER_OUTPUT_CHARS: usize = 768;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveResponseState {
    AwaitingHeaders,
    Streaming,
    NonStreaming,
}

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
    pub response_state: LiveResponseState,
    pub tail: String,
    pub hover_output: String,
    pub started_at: DateTime<Utc>,
    pub terminating: bool,
}

#[derive(Debug, Clone)]
struct LiveRequest {
    meta: LiveRequestMeta,
    response_state: LiveResponseState,
    tail: String,
    hover_output: String,
    started_at: DateTime<Utc>,
    terminate_tx: watch::Sender<bool>,
    terminating: bool,
}

impl LiveRequestStore {
    pub fn start(&self, meta: LiveRequestMeta) -> watch::Receiver<bool> {
        let (terminate_tx, terminate_rx) = watch::channel(false);
        let request = LiveRequest {
            meta: meta.clone(),
            response_state: LiveResponseState::AwaitingHeaders,
            tail: String::new(),
            hover_output: String::new(),
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
        append_hover_output(&mut request.hover_output, delta);
        true
    }

    pub fn confirm_response_kind(&self, id: &str, streaming: bool) -> bool {
        let Ok(mut inner) = self.inner.write() else {
            return false;
        };
        let Some(request) = inner.get_mut(id) else {
            return false;
        };
        if request.response_state != LiveResponseState::AwaitingHeaders {
            return false;
        }
        request.response_state = if streaming {
            LiveResponseState::Streaming
        } else {
            LiveResponseState::NonStreaming
        };
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
                response_state: request.response_state,
                tail: request.tail.clone(),
                hover_output: request.hover_output.clone(),
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

fn append_hover_output(output: &mut String, delta: &str) {
    output.push_str(delta);

    while hover_output_line_count(output) > MAX_HOVER_OUTPUT_LINES {
        pop_hover_output_line(output);
    }

    while output.chars().count() > MAX_HOVER_OUTPUT_CHARS {
        if output.contains('\n') {
            pop_hover_output_line(output);
        } else {
            trim_to_recent_chars(output, MAX_HOVER_OUTPUT_CHARS);
        }
    }
}

fn hover_output_line_count(output: &str) -> usize {
    if output.is_empty() {
        0
    } else {
        output.bytes().filter(|byte| *byte == b'\n').count() + 1
    }
}

fn pop_hover_output_line(output: &mut String) {
    if let Some(line_end) = output.find('\n') {
        output.drain(..=line_end);
    }
}

fn trim_to_recent_chars(text: &mut String, max_chars: usize) {
    let total_chars = text.chars().count();
    let skip_chars = total_chars.saturating_sub(max_chars);
    let split = text
        .char_indices()
        .nth(skip_chars)
        .map(|(index, _)| index)
        .unwrap_or(0);
    if split > 0 {
        text.drain(..split);
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
    fn new_request_awaits_response_headers() {
        let store = LiveRequestStore::default();
        store.start(request_meta());

        assert_eq!(
            store.snapshots()[0].response_state,
            LiveResponseState::AwaitingHeaders
        );
    }

    #[test]
    fn response_kind_can_be_confirmed_as_streaming() {
        let store = LiveRequestStore::default();
        store.start(request_meta());

        assert!(store.confirm_response_kind("request-a", true));
        assert_eq!(
            store.snapshots()[0].response_state,
            LiveResponseState::Streaming
        );
        assert!(!store.confirm_response_kind("request-a", false));
        assert_eq!(
            store.snapshots()[0].response_state,
            LiveResponseState::Streaming
        );
    }

    #[test]
    fn response_kind_can_be_confirmed_as_non_streaming() {
        let store = LiveRequestStore::default();
        store.start(request_meta());

        assert!(store.confirm_response_kind("request-a", false));
        assert_eq!(
            store.snapshots()[0].response_state,
            LiveResponseState::NonStreaming
        );
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

    #[test]
    fn hover_output_appends_raw_deltas_to_the_current_line() {
        let mut output = String::new();

        append_hover_output(&mut output, "first");
        append_hover_output(&mut output, " line\r\nsecond");
        append_hover_output(&mut output, "\tpart");

        assert_eq!(output, "first line\r\nsecond\tpart");
    }

    #[test]
    fn hover_output_keeps_the_latest_ten_lines() {
        let mut output = String::new();
        let input = (0..11)
            .map(|index| format!("line-{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let expected = (1..11)
            .map(|index| format!("line-{index}"))
            .collect::<Vec<_>>()
            .join("\n");

        append_hover_output(&mut output, &input);

        assert_eq!(output, expected);
    }

    #[test]
    fn hover_output_discards_a_complete_line_before_trimming_chars() {
        let mut output = String::new();
        let first = "a".repeat(300);
        let second = "b".repeat(600);

        append_hover_output(&mut output, &format!("{first}\n{second}"));

        assert_eq!(output, second);
    }

    #[test]
    fn hover_output_trims_a_single_line_at_unicode_boundaries() {
        let mut output = String::new();

        append_hover_output(&mut output, &format!("a{}", "界".repeat(768)));

        assert_eq!(output, "界".repeat(768));
        assert_eq!(output.chars().count(), MAX_HOVER_OUTPUT_CHARS);
    }

    #[test]
    fn regular_tail_keeps_its_single_line_behavior() {
        let mut tail = String::new();

        append_tail(&mut tail, "first\nsecond\r\tthird");

        assert_eq!(tail, "first second \tthird");
    }
}
