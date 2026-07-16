use super::LiveOutputRate;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tiktoken_rs::{CoreBPE, bpe_for_model, o200k_base_singleton};

const OUTPUT_RATE_WINDOW: Duration = Duration::from_secs(2);
const MAX_OUTPUT_RATE_CHARS: usize = 65_536;
const MIN_OUTPUT_RATE_SECONDS: f64 = 0.25;

pub(super) struct OutputRateTracker {
    samples: VecDeque<OutputRateSample>,
    sample_chars: usize,
    tokenizer: &'static CoreBPE,
}

struct OutputRateSample {
    recorded_at: Instant,
    text: String,
    char_count: usize,
}

impl OutputRateTracker {
    pub(super) fn new(model: Option<&str>) -> Self {
        Self {
            samples: VecDeque::new(),
            sample_chars: 0,
            tokenizer: tokenizer_for_model(model),
        }
    }

    pub(super) fn append(&mut self, delta: &str, now: Instant) {
        let char_count = delta.chars().count();
        if char_count == 0 {
            return;
        }
        self.samples.push_back(OutputRateSample {
            recorded_at: now,
            text: delta.to_string(),
            char_count,
        });
        self.sample_chars = self.sample_chars.saturating_add(char_count);
        self.prune(now);
    }

    pub(super) fn rate_at(&self, now: Instant) -> Option<LiveOutputRate> {
        estimate_output_rate(&self.samples, self.tokenizer, now)
    }

    fn prune(&mut self, now: Instant) {
        let cutoff = now.checked_sub(OUTPUT_RATE_WINDOW).unwrap_or(now);
        while self
            .samples
            .front()
            .is_some_and(|sample| sample.recorded_at < cutoff)
        {
            if let Some(sample) = self.samples.pop_front() {
                self.sample_chars = self.sample_chars.saturating_sub(sample.char_count);
            }
        }
        while self.sample_chars > MAX_OUTPUT_RATE_CHARS {
            let overflow = self.sample_chars - MAX_OUTPUT_RATE_CHARS;
            let Some(sample) = self.samples.front_mut() else {
                break;
            };
            if sample.char_count <= overflow {
                let sample = self.samples.pop_front().unwrap();
                self.sample_chars = self.sample_chars.saturating_sub(sample.char_count);
                continue;
            }
            trim_leading_chars(&mut sample.text, overflow);
            sample.char_count -= overflow;
            self.sample_chars -= overflow;
        }
    }
}

fn estimate_output_rate(
    samples: &VecDeque<OutputRateSample>,
    tokenizer: &CoreBPE,
    now: Instant,
) -> Option<LiveOutputRate> {
    let first_sample = samples.front()?;
    let cutoff = now.checked_sub(OUTPUT_RATE_WINDOW).unwrap_or(now);
    let mut text = String::new();
    let mut chars = 0;
    let mut rate_started_at = None;
    for sample in samples
        .iter()
        .filter(|sample| sample.recorded_at >= cutoff)
    {
        rate_started_at.get_or_insert(sample.recorded_at);
        text.push_str(&sample.text);
        chars += sample.char_count;
    }
    let rate_started_at = rate_started_at.unwrap_or_else(|| first_sample.recorded_at.max(cutoff));
    let seconds = now
        .saturating_duration_since(rate_started_at)
        .as_secs_f64()
        .max(MIN_OUTPUT_RATE_SECONDS)
        .min(OUTPUT_RATE_WINDOW.as_secs_f64());
    let tokens = if text.is_empty() {
        0
    } else {
        tokenizer.count_ordinary(&text)
    };
    Some(LiveOutputRate {
        estimated_tokens_per_second: tokens as f64 / seconds,
        chars_per_second: chars as f64 / seconds,
    })
}

fn tokenizer_for_model(model: Option<&str>) -> &'static CoreBPE {
    let Some(model) = model
        .map(normalize_tokenizer_model)
        .filter(|model| !model.is_empty())
    else {
        return o200k_base_singleton();
    };
    match bpe_for_model(model) {
        Ok(tokenizer) => tokenizer,
        Err(err) => {
            tracing::debug!(model, error = %err, "using o200k tokenizer fallback for live output");
            o200k_base_singleton()
        }
    }
}

fn normalize_tokenizer_model(model: &str) -> &str {
    model.trim().strip_prefix("openai/").unwrap_or(model.trim())
}

fn trim_leading_chars(text: &mut String, count: usize) {
    let split = text
        .char_indices()
        .nth(count)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    if split > 0 {
        text.drain(..split);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_rate_uses_recent_delta_text() {
        let started = Instant::now();
        let samples = VecDeque::from([OutputRateSample {
            recorded_at: started,
            text: "hello world".to_string(),
            char_count: 11,
        }]);

        let rate = estimate_output_rate(
            &samples,
            o200k_base_singleton(),
            started + Duration::from_secs(1),
        )
        .unwrap();

        assert_eq!(rate.estimated_tokens_per_second, 2.0);
        assert_eq!(rate.chars_per_second, 11.0);
    }

    #[test]
    fn output_rate_drops_to_zero_after_window() {
        let started = Instant::now();
        let samples = VecDeque::from([OutputRateSample {
            recorded_at: started,
            text: "hello".to_string(),
            char_count: 5,
        }]);

        let rate = estimate_output_rate(
            &samples,
            o200k_base_singleton(),
            started + Duration::from_secs(3),
        )
        .unwrap();

        assert_eq!(rate.estimated_tokens_per_second, 0.0);
        assert_eq!(rate.chars_per_second, 0.0);
    }

    #[test]
    fn tokenizer_model_removes_openai_prefix_and_falls_back() {
        assert_eq!(normalize_tokenizer_model(" openai/gpt-5 "), "gpt-5");
        assert!(std::ptr::eq(
            tokenizer_for_model(Some("unknown-provider-model")),
            o200k_base_singleton()
        ));
    }
}
