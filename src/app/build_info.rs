use std::sync::OnceLock;

const BUILD_TAG: &str = env!("CODEX_SWITCH_BUILD_TAG");
const BUILD_COMMIT: &str = env!("CODEX_SWITCH_BUILD_COMMIT");
const BUILD_EXACT_TAG: &str = env!("CODEX_SWITCH_BUILD_EXACT_TAG");
const BUILD_DIRTY: &str = env!("CODEX_SWITCH_BUILD_DIRTY");

pub fn display_version() -> &'static str {
    static DISPLAY_VERSION: OnceLock<String> = OnceLock::new();
    DISPLAY_VERSION
        .get_or_init(|| {
            format_build_version(
                BUILD_TAG,
                (!BUILD_COMMIT.is_empty()).then_some(BUILD_COMMIT),
                BUILD_EXACT_TAG == "1",
                BUILD_DIRTY == "1",
            )
        })
        .as_str()
}

fn format_build_version(tag: &str, commit: Option<&str>, exact_tag: bool, dirty: bool) -> String {
    let Some(commit) = commit else {
        return tag.to_string();
    };
    if dirty {
        return format!("{tag}^{commit}");
    }
    if exact_tag {
        return tag.to_string();
    }
    format!("{tag}+{commit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_clean_tag_uses_the_tag_only() {
        assert_eq!(
            format_build_version("v0.4.0", Some("abc123"), true, false),
            "v0.4.0"
        );
    }

    #[test]
    fn clean_non_tag_build_includes_its_commit() {
        assert_eq!(
            format_build_version("v0.4.0", Some("abc123"), false, false),
            "v0.4.0+abc123"
        );
    }

    #[test]
    fn dirty_build_marks_its_commit_with_a_caret() {
        assert_eq!(
            format_build_version("v0.4.0", Some("abc123"), true, true),
            "v0.4.0^abc123"
        );
    }
}
