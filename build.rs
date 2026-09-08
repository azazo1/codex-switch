use std::{env, path::PathBuf};

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
