use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tokio::sync::watch;

mod rate;

use rate::OutputRateTracker;

const MAX_TAIL_CHARS: usize = 16_384;
const MAX_HOVER_OUTPUT_LINES: usize = 10;
const MAX_HOVER_OUTPUT_CHARS: usize = 768;

pub const MIN_SCROLL_CHARS_PER_SECOND: u32 = 1;
pub const MAX_SCROLL_CHARS_PER_SECOND: u32 = 1_000;
pub const MAX_COMPLETED_HOLD_SECONDS: u32 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveOutputSettings {
    pub scroll_limit_enabled: bool,
    pub max_scroll_chars_per_second: u32,
    pub completed_hold_seconds: u32,
}

impl Default for LiveOutputSettings {
    fn default() -> Self {
        Self {
            scroll_limit_enabled: false,
            max_scroll_chars_per_second: 60,
            completed_hold_seconds: 2,
        }
    }
}

impl LiveOutputSettings {
    pub fn normalized(self) -> Self {
        Self {
            scroll_limit_enabled: self.scroll_limit_enabled,
            max_scroll_chars_per_second: self
                .max_scroll_chars_per_second
                .clamp(MIN_SCROLL_CHARS_PER_SECOND, MAX_SCROLL_CHARS_PER_SECOND),
            completed_hold_seconds: self
                .completed_hold_seconds
                .min(MAX_COMPLETED_HOLD_SECONDS),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveOutputRate {
    pub estimated_tokens_per_second: f64,
    pub chars_per_second: f64,
}

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
    pub tail_start_char_index: usize,
    pub tail_end_char_index: usize,
    pub hover_output: String,
    pub output_rate: Option<LiveOutputRate>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub terminating: bool,
}

struct LiveRequest {
    meta: LiveRequestMeta,
    response_state: LiveResponseState,
    tail: String,
    tail_start_char_index: usize,
    tail_end_char_index: usize,
    hover_output: String,
    output_rate: OutputRateTracker,
    frozen_output_rate: Option<LiveOutputRate>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    terminate_tx: watch::Sender<bool>,
    terminating: bool,
}

impl LiveRequestStore {
    pub fn start(&self, meta: LiveRequestMeta) -> watch::Receiver<bool> {
        let (terminate_tx, terminate_rx) = watch::channel(false);
        let request = LiveRequest {
            output_rate: OutputRateTracker::new(meta.model.as_deref()),
            meta: meta.clone(),
            response_state: LiveResponseState::AwaitingHeaders,
            tail: String::new(),
            tail_start_char_index: 0,
            tail_end_char_index: 0,
            hover_output: String::new(),
            frozen_output_rate: None,
            started_at: Utc::now(),
            finished_at: None,
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
        if request.finished_at.is_some() {
            return false;
        }
        append_tail(
            &mut request.tail,
            &mut request.tail_start_char_index,
            &mut request.tail_end_char_index,
            delta,
        );
        append_hover_output(&mut request.hover_output, delta);
        request.output_rate.append(delta, Instant::now());
        true
    }

    pub fn confirm_response_kind(&self, id: &str, streaming: bool) -> bool {
        let Ok(mut inner) = self.inner.write() else {
            return false;
        };
        let Some(request) = inner.get_mut(id) else {
            return false;
        };
        if request.finished_at.is_some()
            || request.response_state != LiveResponseState::AwaitingHeaders
        {
            return false;
        }
        request.response_state = if streaming {
            LiveResponseState::Streaming
        } else {
            LiveResponseState::NonStreaming
        };
        true
    }

    pub fn finish(&self, id: &str) -> bool {
        let Ok(mut inner) = self.inner.write() else {
            return false;
        };
        let Some(request) = inner.get_mut(id) else {
            return false;
        };
        if request.finished_at.is_some() {
            return false;
        }
        let now = Instant::now();
        request.frozen_output_rate = request.output_rate.rate_at(now);
        request.finished_at = Some(Utc::now());
        true
    }

    pub fn remove_finished(&self, id: &str) -> bool {
        let Ok(mut inner) = self.inner.write() else {
            return false;
        };
        let finished = inner
            .get(id)
            .is_some_and(|request| request.finished_at.is_some());
        if finished {
            inner.remove(id);
        }
        finished
    }

    pub fn terminate(&self, id: &str) -> bool {
        let Ok(mut inner) = self.inner.write() else {
            return false;
        };
        let Some(request) = inner.get_mut(id) else {
            return false;
        };
        if request.finished_at.is_some() {
            return false;
        }
        request.terminating = true;
        request.terminate_tx.send_replace(true);
        true
    }

    pub fn snapshots(&self) -> Vec<LiveRequestSnapshot> {
        let Ok(inner) = self.inner.read() else {
            return Vec::new();
        };
        let now = Instant::now();
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
                tail_start_char_index: request.tail_start_char_index,
                tail_end_char_index: request.tail_end_char_index,
                hover_output: request.hover_output.clone(),
                output_rate: request
                    .frozen_output_rate
                    .or_else(|| request.output_rate.rate_at(now)),
                started_at: request.started_at,
                finished_at: request.finished_at,
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

fn append_tail(
    tail: &mut String,
    tail_start_char_index: &mut usize,
    tail_end_char_index: &mut usize,
    delta: &str,
) {
    let mut appended_chars = 0;
    for ch in delta.chars() {
        if ch == '\n' || ch == '\r' {
            tail.push(' ');
        } else {
            tail.push(ch);
        }
        appended_chars += 1;
    }
    *tail_end_char_index = (*tail_end_char_index).saturating_add(appended_chars);
    let tail_chars = tail.chars().count();
    if tail_chars <= MAX_TAIL_CHARS {
        return;
    }
    let drop_chars = tail_chars - MAX_TAIL_CHARS;
    let split = tail
        .char_indices()
        .nth(drop_chars)
        .map(|(index, _)| index)
        .unwrap_or(tail.len());
    if split > 0 {
        tail.drain(..split);
        *tail_start_char_index = (*tail_start_char_index).saturating_add(drop_chars);
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
            model: Some("gpt-5".to_string()),
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
    fn finish_keeps_request_until_ui_removes_it() {
        let store = LiveRequestStore::default();
        store.start(request_meta());

        assert!(store.finish("request-a"));
        assert!(store.snapshots()[0].finished_at.is_some());
        assert!(!store.terminate("request-a"));
        assert!(store.remove_finished("request-a"));
        assert!(store.snapshots().is_empty());
    }

    #[test]
    fn active_request_cannot_be_removed_as_finished() {
        let store = LiveRequestStore::default();
        store.start(request_meta());

        assert!(!store.remove_finished("request-a"));
        assert_eq!(store.snapshots().len(), 1);
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
    fn regular_tail_keeps_unicode_character_indexes() {
        let mut tail = String::new();
        let mut start = 0;
        let mut end = 0;

        append_tail(&mut tail, &mut start, &mut end, "first\n界");

        assert_eq!(tail, "first 界");
        assert_eq!(start, 0);
        assert_eq!(end, 7);
    }

    #[test]
    fn regular_tail_keeps_latest_sixteen_k_characters() {
        let mut tail = String::new();
        let mut start = 0;
        let mut end = 0;

        append_tail(
            &mut tail,
            &mut start,
            &mut end,
            &format!("a{}", "界".repeat(MAX_TAIL_CHARS)),
        );

        assert_eq!(tail.chars().count(), MAX_TAIL_CHARS);
        assert_eq!(start, 1);
        assert_eq!(end, MAX_TAIL_CHARS + 1);
    }
}
