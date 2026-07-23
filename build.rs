use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("Cargo must set CARGO_MANIFEST_DIR"),
    );
    emit_rerun_hints(&manifest_dir);

    let metadata = GitMetadata::collect(&manifest_dir);
    println!("cargo:rustc-env=CODEX_SWITCH_BUILD_TAG={}", metadata.tag);
    println!(
        "cargo:rustc-env=CODEX_SWITCH_BUILD_COMMIT={}",
        metadata.commit.unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=CODEX_SWITCH_BUILD_EXACT_TAG={}",
        u8::from(metadata.exact_tag)
    );
    println!(
        "cargo:rustc-env=CODEX_SWITCH_BUILD_DIRTY={}",
        u8::from(metadata.dirty)
    );
}

struct GitMetadata {
    tag: String,
    commit: Option<String>,
    exact_tag: bool,
    dirty: bool,
}

impl GitMetadata {
    fn collect(manifest_dir: &Path) -> Self {
        let fallback_tag = format!(
            "v{}",
            env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string())
        );
        let exact_tag = git_output(manifest_dir, &["tag", "--points-at", "HEAD"])
            .and_then(|tags| select_version_tag(&tags));
        let tag = exact_tag.clone().unwrap_or_else(|| {
            git_output(manifest_dir, &["describe", "--tags", "--abbrev=0", "HEAD"])
                .unwrap_or(fallback_tag)
        });
        let commit = git_output(manifest_dir, &["rev-parse", "--short=12", "HEAD"]);
        let dirty = commit.is_some() && git_is_dirty(manifest_dir);

        Self {
            tag,
            commit,
            exact_tag: exact_tag.is_some(),
            dirty,
        }
    }
}

fn emit_rerun_hints(manifest_dir: &Path) {
    println!("cargo:rerun-if-changed=build.rs");

    if let Some(files) = git_output(manifest_dir, &["ls-files"]) {
        for file in files.lines() {
            println!("cargo:rerun-if-changed={}", manifest_dir.join(file).display());
        }
    }

    let Some(git_dir) = git_output(manifest_dir, &["rev-parse", "--git-dir"]) else {
        return;
    };
    let git_dir = PathBuf::from(git_dir);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        manifest_dir.join(git_dir)
    };
    for path in ["HEAD", "index", "packed-refs", "refs"] {
        println!("cargo:rerun-if-changed={}", git_dir.join(path).display());
    }
}

fn select_version_tag(tags: &str) -> Option<String> {
    tags.lines()
        .find(|tag| tag.starts_with('v'))
        .or_else(|| tags.lines().next())
        .map(ToOwned::to_owned)
}

fn git_output(manifest_dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(manifest_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let output = String::from_utf8(output.stdout).ok()?;
    let output = output.trim();
    (!output.is_empty()).then(|| output.to_owned())
}

fn git_is_dirty(manifest_dir: &Path) -> bool {
    Command::new("git")
        .args(["diff-index", "--quiet", "HEAD", "--"])
        .current_dir(manifest_dir)
        .status()
        .ok()
        .is_some_and(|status| status.code() == Some(1))
}
