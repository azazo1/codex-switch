use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::{env, fmt::Write as _};

const WINDOWS_ICON_PATH: &str = "assets/app-icon.ico";
const BUILD_VERSION_ENV: &str = "CODEX_SWITCH_BUILD_VERSION";

fn main() {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("Cargo must set CARGO_MANIFEST_DIR"),
    );
    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(WINDOWS_ICON_PATH).display()
    );
    // 日常开发构建不设置该变量, 因此不读取 .git, 增量缓存不因 commit 推进而失效.
    println!("cargo:rerun-if-env-changed={BUILD_VERSION_ENV}");
    embed_windows_resources();
    embed_pricing_presets(&manifest_dir);

    // 版本注入默认关闭, 仅发布构建路径 (just dist 与 CI) 设置该变量, 日常开发构建显示 dev-build.
    let injected = env::var(BUILD_VERSION_ENV).unwrap_or_default();
    let injected = injected.trim();
    let build_version = if injected.is_empty() {
        "dev-build"
    } else {
        injected
    };
    println!("cargo:rustc-env={BUILD_VERSION_ENV}={build_version}");
}

fn embed_windows_resources() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    winresource::WindowsResource::new()
        .set_icon(WINDOWS_ICON_PATH)
        .compile()
        .expect("failed to compile Windows application icon");
}

/// 扫描 examples/*.rhai, 生成编译期嵌入的计价预设.
///
/// 文件名 `foo_bar.rhai` 默认 id 为 `foo-bar`. 文件开头可用:
/// `// @label 显示名`, `// @id 自定义-id`, `// @prefer https://host/v1`.
fn embed_pricing_presets(manifest_dir: &Path) {
    let examples_dir = manifest_dir.join("examples");
    println!("cargo:rerun-if-changed={}", examples_dir.display());

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"));
    let examples_out = out_dir.join("pricing_examples");
    if examples_out.exists() {
        fs::remove_dir_all(&examples_out)
            .unwrap_or_else(|error| panic!("failed to reset {}: {error}", examples_out.display()));
    }
    fs::create_dir_all(&examples_out).unwrap_or_else(|error| {
        panic!("failed to create {}: {error}", examples_out.display());
    });

    let mut files = Vec::new();
    if examples_dir.is_dir() {
        for entry in fs::read_dir(&examples_dir).unwrap_or_else(|error| {
            panic!("failed to read {}: {error}", examples_dir.display());
        }) {
            let entry = entry.unwrap_or_else(|error| {
                panic!("failed to read {} entry: {error}", examples_dir.display());
            });
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("rhai") {
                continue;
            }
            println!("cargo:rerun-if-changed={}", path.display());
            files.push(path);
        }
    }
    files.sort();

    let mut rust = String::from("pub const PRESETS: &[PricingPreset] = &[\n");
    rust.push_str(
        "    PricingPreset {\n        id: PRESET_DEFAULT,\n        label: \"默认\",\n        source: DEFAULT_SCRIPT,\n        prefer: &[],\n    },\n",
    );

    let mut seen_ids = HashSet::from(["default".to_string()]);
    let mut seen_prefers = HashSet::new();
    for path in &files {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_else(|| panic!("preset filename is not utf-8: {}", path.display()));
        if !is_safe_stem(stem) {
            panic!(
                "preset filename must be [A-Za-z0-9_-]+: {}",
                path.display()
            );
        }
        let source = fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        let (meta, body) = parse_preset_source(&source, stem, path);
        if !seen_ids.insert(meta.id.clone()) {
            panic!(
                "duplicate preset id `{}` from {}",
                meta.id,
                path.display()
            );
        }
        for prefer in &meta.prefer {
            let key = normalize_prefer_url(prefer);
            if !seen_prefers.insert(key) {
                panic!(
                    "duplicate @prefer `{prefer}` from {}",
                    path.display()
                );
            }
        }
        let out_name = format!("{stem}.rhai");
        fs::write(examples_out.join(&out_name), body).unwrap_or_else(|error| {
            panic!("failed to write {}: {error}", examples_out.join(&out_name).display());
        });
        write!(
            rust,
            "    PricingPreset {{\n        id: \"{}\",\n        label: \"{}\",\n        source: include_str!(concat!(env!(\"OUT_DIR\"), \"/pricing_examples/{out_name}\")),\n        prefer: {},\n    }},\n",
            escape_rust_str(&meta.id),
            escape_rust_str(&meta.label),
            rust_str_slice(&meta.prefer),
        )
        .expect("writing preset source");
    }
    rust.push_str("];\n");

    fs::write(out_dir.join("pricing_presets.rs"), rust).unwrap_or_else(|error| {
        panic!("failed to write pricing_presets.rs: {error}");
    });
}

struct PresetMeta {
    id: String,
    label: String,
    prefer: Vec<String>,
}

fn parse_preset_source(source: &str, stem: &str, path: &Path) -> (PresetMeta, String) {
    let mut id = None;
    let mut label = None;
    let mut prefer = Vec::new();
    let mut body = Vec::new();
    let mut in_header = true;
    for line in source.lines() {
        let trimmed = line.trim();
        if in_header && trimmed.starts_with("// @") {
            let directive = trimmed.trim_start_matches("// @").trim();
            let (key, value) = parse_directive(directive);
            match key {
                "label" => {
                    if value.is_empty() {
                        panic!("empty @label in {}", path.display());
                    }
                    label = Some(value.to_string());
                }
                "id" => {
                    if value.is_empty() {
                        panic!("empty @id in {}", path.display());
                    }
                    if !is_safe_id(value) {
                        panic!("invalid @id `{value}` in {}", path.display());
                    }
                    id = Some(value.to_string());
                }
                "prefer" => {
                    if value.is_empty() {
                        panic!("empty @prefer in {}", path.display());
                    }
                    if !value.starts_with("http://") && !value.starts_with("https://") {
                        panic!("@prefer must be an http(s) URL in {}", path.display());
                    }
                    if value.chars().any(char::is_whitespace) {
                        panic!("invalid @prefer `{value}` in {}", path.display());
                    }
                    prefer.push(value.to_string());
                }
                other => panic!("unknown preset directive `@{other}` in {}", path.display()),
            }
            continue;
        }
        if in_header && trimmed.is_empty() {
            continue;
        }
        in_header = false;
        body.push(line);
    }

    let mut body = body.join("\n");
    if source.ends_with('\n') && !body.ends_with('\n') {
        body.push('\n');
    }
    let id = id.unwrap_or_else(|| stem.replace('_', "-"));
    if !is_safe_id(&id) {
        panic!("invalid preset id `{id}` from {}", path.display());
    }
    let label = label.unwrap_or_else(|| id.clone());
    (
        PresetMeta { id, label, prefer },
        body,
    )
}

fn parse_directive(directive: &str) -> (&str, &str) {
    if let Some((key, value)) = directive.split_once(char::is_whitespace) {
        (key.trim_end_matches(':'), value.trim())
    } else if let Some((key, value)) = directive.split_once(':') {
        (key, value.trim())
    } else {
        (directive, "")
    }
}

fn is_safe_stem(stem: &str) -> bool {
    !stem.is_empty()
        && stem
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

fn escape_rust_str(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn rust_str_slice(values: &[String]) -> String {
    if values.is_empty() {
        return "&[]".to_string();
    }
    let inner = values
        .iter()
        .map(|value| format!("\"{}\"", escape_rust_str(value)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("&[{inner}]")
}

fn normalize_prefer_url(value: &str) -> String {
    value.trim().trim_end_matches('/').to_ascii_lowercase()
}
