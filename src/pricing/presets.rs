use super::DEFAULT_SCRIPT;

pub const PRESET_DEFAULT: &str = "default";

#[derive(Debug, Clone, Copy)]
pub struct PricingPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub source: &'static str,
    pub prefer: &'static [&'static str],
}

include!(concat!(env!("OUT_DIR"), "/pricing_presets.rs"));

pub fn preset(id: &str) -> Option<&'static PricingPreset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

pub fn preferred_preset_for_base_url(base_url: &str) -> Option<&'static PricingPreset> {
    let url = normalize_base_url(base_url);
    if url.is_empty() {
        return None;
    }
    PRESETS.iter().find(|preset| {
        preset
            .prefer
            .iter()
            .any(|prefer| base_url_matches(&url, &normalize_base_url(prefer)))
    })
}

fn normalize_base_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_ascii_lowercase()
}

fn base_url_matches(left: &str, right: &str) -> bool {
    prefix_at_slash_boundary(left, right) || prefix_at_slash_boundary(right, left)
}

fn prefix_at_slash_boundary(full: &str, prefix: &str) -> bool {
    full == prefix
        || (full.starts_with(prefix) && full.as_bytes().get(prefix.len()) == Some(&b'/'))
}

#[cfg(test)]
mod tests {
    use super::super::compile_check;
    use super::*;

    #[test]
    fn presets_compile() {
        for item in PRESETS {
            compile_check(item.source).unwrap_or_else(|error| {
                panic!("preset {} failed to compile: {error}", item.id);
            });
        }
    }

    #[test]
    fn default_preset_is_first() {
        assert_eq!(PRESETS[0].id, PRESET_DEFAULT);
        assert_eq!(PRESETS[0].source, super::DEFAULT_SCRIPT);
        assert!(PRESETS[0].prefer.is_empty());
    }

    #[test]
    fn prefers_preset_for_matching_base_url() {
        let preset = preferred_preset_for_base_url("https://api.deepseek.ai/v1")
            .expect("deepseek official preset");
        assert_eq!(preset.id, "deepseek-official");
        assert!(preferred_preset_for_base_url("https://api.deepseek.ai/v1/").is_some());
        assert!(preferred_preset_for_base_url("https://api.deepseek.ai").is_some());
        assert!(preferred_preset_for_base_url("HTTPS://API.DEEPSEEK.AI/V1").is_some());
        assert!(preferred_preset_for_base_url("https://example.test").is_none());
        assert!(preferred_preset_for_base_url("").is_none());
    }
}
