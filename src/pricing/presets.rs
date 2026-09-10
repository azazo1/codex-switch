use super::compile_check;
use super::DEFAULT_SCRIPT;

pub const PRESET_DEFAULT: &str = "default";
pub const PRESET_DEEPSEEK_OFFICIAL: &str = "deepseek-official";

#[derive(Debug, Clone, Copy)]
pub struct PricingPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub source: &'static str,
}

pub const PRESETS: &[PricingPreset] = &[
    PricingPreset {
        id: PRESET_DEFAULT,
        label: "默认",
        source: DEFAULT_SCRIPT,
    },
    PricingPreset {
        id: PRESET_DEEPSEEK_OFFICIAL,
        label: "DeepSeek 官方价",
        source: include_str!("../../examples/deepseek_official.rhai"),
    },
];

pub fn preset(id: &str) -> Option<&'static PricingPreset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_compile() {
        for item in PRESETS {
            compile_check(item.source).unwrap_or_else(|error| {
                panic!("preset {} failed to compile: {error}", item.id);
            });
        }
    }
}
