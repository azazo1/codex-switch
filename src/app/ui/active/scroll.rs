use crate::live::{LiveOutputSettings, LiveRequestSnapshot};
use std::time::{Duration, Instant};

const DEFAULT_WINDOW_CHARS: usize = 22;

pub(in crate::app::ui) struct LiveTailScrollState {
    visible_end_char_index: f64,
    target_end_char_index: usize,
    window_chars: usize,
    last_advanced_at: Instant,
    reached_end_at: Option<Instant>,
}

impl LiveTailScrollState {
    pub(super) fn new(
        item: &LiveRequestSnapshot,
        settings: LiveOutputSettings,
        now: Instant,
    ) -> Self {
        let available_chars = item
            .tail_end_char_index
            .saturating_sub(item.tail_start_char_index);
        let visible_end_char_index = if !settings.scroll_limit_enabled
            || available_chars <= DEFAULT_WINDOW_CHARS
        {
            item.tail_end_char_index as f64
        } else {
            (item.tail_start_char_index + DEFAULT_WINDOW_CHARS) as f64
        };
        let reached_end_at = (item.finished_at.is_some()
            && visible_end_char_index >= item.tail_end_char_index as f64)
            .then_some(now);
        Self {
            visible_end_char_index,
            target_end_char_index: item.tail_end_char_index,
            window_chars: DEFAULT_WINDOW_CHARS,
            last_advanced_at: now,
            reached_end_at,
        }
    }

    pub(super) fn sync(
        &mut self,
        item: &LiveRequestSnapshot,
        settings: LiveOutputSettings,
        now: Instant,
    ) {
        self.advance(settings, now);
        self.target_end_char_index = item.tail_end_char_index;
        let earliest_full_window_end = item
            .tail_start_char_index
            .saturating_add(self.window_chars)
            .min(self.target_end_char_index);
        self.visible_end_char_index = self
            .visible_end_char_index
            .max(earliest_full_window_end as f64)
            .min(self.target_end_char_index as f64);
        if !settings.scroll_limit_enabled {
            self.visible_end_char_index = self.target_end_char_index as f64;
        }
        self.update_reached_end(item.finished_at.is_some(), now);
    }

    pub(super) fn advance(&mut self, settings: LiveOutputSettings, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_advanced_at);
        self.last_advanced_at = now;
        if settings.scroll_limit_enabled {
            self.visible_end_char_index = (self.visible_end_char_index
                + elapsed.as_secs_f64() * settings.max_scroll_chars_per_second as f64)
                .min(self.target_end_char_index as f64);
        } else {
            self.visible_end_char_index = self.target_end_char_index as f64;
        }
    }

    pub(super) fn set_window_chars(
        &mut self,
        item: &LiveRequestSnapshot,
        max_chars: usize,
        settings: LiveOutputSettings,
        now: Instant,
    ) {
        self.window_chars = max_chars.max(8);
        let earliest_full_window_end = item
            .tail_start_char_index
            .saturating_add(self.window_chars)
            .min(item.tail_end_char_index);
        self.visible_end_char_index = self
            .visible_end_char_index
            .max(earliest_full_window_end as f64)
            .min(item.tail_end_char_index as f64);
        if !settings.scroll_limit_enabled {
            self.visible_end_char_index = item.tail_end_char_index as f64;
        }
        self.update_reached_end(item.finished_at.is_some(), now);
    }

    pub(super) fn update_reached_end(&mut self, finished: bool, now: Instant) {
        if finished && !self.needs_scroll() {
            self.reached_end_at.get_or_insert(now);
        } else {
            self.reached_end_at = None;
        }
    }

    pub(super) fn visible_end_char_index(&self) -> f64 {
        self.visible_end_char_index
    }

    pub(super) fn needs_scroll(&self) -> bool {
        self.visible_end_char_index.floor() < self.target_end_char_index as f64
    }

    pub(super) fn hold_finished(&self, now: Instant, hold_seconds: u32) -> bool {
        self.reached_end_at.is_some_and(|reached| {
            now.saturating_duration_since(reached) >= Duration::from_secs(hold_seconds as u64)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::{LiveOutputRate, LiveResponseState};
    use chrono::Utc;

    fn snapshot(tail: &str, finished: bool) -> LiveRequestSnapshot {
        LiveRequestSnapshot {
            id: "request-a".to_string(),
            upstream_name: None,
            endpoint: "/responses".to_string(),
            model: Some("gpt-5".to_string()),
            reasoning_effort: None,
            response_state: LiveResponseState::Streaming,
            tail: tail.to_string(),
            tail_start_char_index: 0,
            tail_end_char_index: tail.chars().count(),
            hover_output: tail.to_string(),
            output_rate: Some(LiveOutputRate {
                estimated_tokens_per_second: 10.0,
                chars_per_second: 40.0,
            }),
            started_at: Utc::now(),
            finished_at: finished.then(Utc::now),
            terminating: false,
        }
    }

    #[test]
    fn limited_scroll_advances_by_configured_character_rate() {
        let now = Instant::now();
        let item = snapshot(&"a".repeat(200), false);
        let settings = LiveOutputSettings {
            scroll_limit_enabled: true,
            ..LiveOutputSettings::default()
        };
        let mut scroll = LiveTailScrollState::new(&item, settings, now);

        scroll.advance(settings, now + Duration::from_secs(1));

        assert_eq!(scroll.visible_end_char_index(), 82.0);
    }

    #[test]
    fn caught_up_scroll_does_not_bank_idle_time() {
        let now = Instant::now();
        let settings = LiveOutputSettings {
            scroll_limit_enabled: true,
            ..LiveOutputSettings::default()
        };
        let first = snapshot(&"a".repeat(20), false);
        let mut scroll = LiveTailScrollState::new(&first, settings, now);
        let later = now + Duration::from_secs(10);
        scroll.advance(settings, later);
        let next = snapshot(&"a".repeat(200), false);

        scroll.sync(&next, settings, later);

        assert_eq!(scroll.visible_end_char_index(), 22.0);
    }

    #[test]
    fn closed_connection_waits_for_scroll_and_hold_time() {
        let now = Instant::now();
        let item = snapshot(&"a".repeat(100), true);
        let settings = LiveOutputSettings {
            scroll_limit_enabled: true,
            max_scroll_chars_per_second: 100,
            completed_hold_seconds: 2,
        };
        let mut scroll = LiveTailScrollState::new(&item, settings, now);

        assert!(!scroll.hold_finished(now + Duration::from_secs(2), 2));
        let reached = now + Duration::from_secs(1);
        scroll.advance(settings, reached);
        scroll.update_reached_end(true, reached);

        assert!(!scroll.hold_finished(reached + Duration::from_secs(1), 2));
        assert!(scroll.hold_finished(reached + Duration::from_secs(2), 2));
    }
}
