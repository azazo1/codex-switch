use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, BufWriter, Write as _},
    path::Path,
    time::{Duration, Instant},
};

use eframe::{Storage, egui};
use serde::{Deserialize, Serialize};

pub const DEFAULT_WINDOW_SIZE: egui::Vec2 = egui::vec2(780.0, 560.0);

const WINDOW_KEY: &str = "window";

/// 窗口可见后等待多久再发起原生全屏切换.
///
/// eframe 总是先以隐藏状态创建窗口, 等首帧绘制完才显示, 之后系统还要处理显示动画,
/// 这段时间内 toggleFullScreen 会被拒绝, 所以需要稍等片刻再切换.
const FULLSCREEN_FIRST_DELAY: Duration = Duration::from_millis(500);
/// 一次全屏切换失败后的重试间隔.
const FULLSCREEN_RETRY_INTERVAL: Duration = Duration::from_millis(700);
/// 全屏切换的最大尝试次数, 超过后放弃并留给用户手动切换.
const FULLSCREEN_MAX_ATTEMPTS: u32 = 5;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PersistedWindowSettings {
    inner_position_pixels: Option<egui::Pos2>,
    outer_position_pixels: Option<egui::Pos2>,
    fullscreen: bool,
    maximized: bool,
    inner_size_points: Option<egui::Vec2>,
}

impl PersistedWindowSettings {
    pub fn load(storage: &dyn Storage) -> Option<Self> {
        eframe::get_value(storage, WINDOW_KEY)
    }

    pub fn is_valid(&self) -> bool {
        let size_ok = self.inner_size_points.is_some_and(|size| {
            size.x > 0.0 && size.y > 0.0 && size.x.is_finite() && size.y.is_finite()
        });
        let position_ok = self
            .inner_position_pixels
            .is_none_or(|pos| !is_placeholder_position(pos))
            && self
                .outer_position_pixels
                .is_none_or(|pos| !is_placeholder_position(pos));
        size_ok && position_ok
    }
}

// Windows 用 (-32000, -32000) 表示尚未真正显示的隐藏窗口, 这种状态不能持久化.
pub fn sanitize_file(path: &Path) {
    let Ok(file) = File::open(path) else {
        return;
    };
    let Ok(mut kv) = ron::de::from_reader::<_, HashMap<String, String>>(BufReader::new(file))
    else {
        return;
    };
    let Some(raw) = kv.get(WINDOW_KEY) else {
        return;
    };
    let invalid = match ron::from_str::<PersistedWindowSettings>(raw) {
        Ok(settings) => !settings.is_valid(),
        Err(_) => true,
    };
    if !invalid {
        return;
    }

    kv.remove(WINDOW_KEY);
    tracing::warn!(
        "removed invalid persisted window state from {}",
        path.display()
    );
    write_kv(path, &kv);
}

// eframe 会在隐藏窗口状态下保存占位几何, 这里在写入磁盘前把它过滤掉.
pub fn sanitize_on_save(
    storage: &mut dyn Storage,
    last_good: Option<&PersistedWindowSettings>,
) -> Option<PersistedWindowSettings> {
    let Some(current) = PersistedWindowSettings::load(storage) else {
        return last_good.cloned();
    };
    if current.is_valid() {
        return Some(current);
    }

    tracing::warn!("ignoring invalid persisted window state");
    if let Some(last_good) = last_good {
        eframe::set_value(storage, WINDOW_KEY, last_good);
        Some(last_good.clone())
    } else {
        storage.remove_string(WINDOW_KEY);
        None
    }
}

/// 取出持久化状态里的全屏标志并清除, 返回是否需要在窗口可见后恢复.
///
/// macOS 上不能带着 `fullscreen` 创建窗口: eframe 固定以隐藏状态创建窗口, 而 winit 的
/// `set_fullscreen` 此时就会调用 toggleFullScreen, 于是这次切换会失败; winit 会每 0.5 秒
/// 重试直到成功, 结果窗口刚显示出来就被系统拖进新的全屏 space, 看起来就像 "闪一下就消失".
/// 因此启动前先清掉该标志, 由应用在窗口可见后主动恢复.
#[cfg(target_os = "macos")]
pub fn take_initial_fullscreen(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let Ok(mut kv) = ron::de::from_reader::<_, HashMap<String, String>>(BufReader::new(file))
    else {
        return false;
    };
    let Some(raw) = kv.get(WINDOW_KEY) else {
        return false;
    };
    let Ok(mut settings) = ron::from_str::<PersistedWindowSettings>(raw) else {
        return false;
    };
    if !settings.fullscreen {
        return false;
    }

    settings.fullscreen = false;
    let Ok(serialized) = ron::ser::to_string(&settings) else {
        return false;
    };
    kv.insert(WINDOW_KEY.to_owned(), serialized);
    write_kv(path, &kv);
    tracing::info!("cleared persisted fullscreen flag, restoring it after the window shows up");
    true
}

/// 原生全屏的延迟恢复状态.
///
/// 只有在窗口真正可见之后才发起切换, 并在切换被系统拒绝时重试若干次.
#[derive(Debug, Default)]
pub struct DeferredFullscreen {
    pending: bool,
    next_attempt_at: Option<Instant>,
    attempts: u32,
}

impl DeferredFullscreen {
    pub fn new(pending: bool) -> Self {
        Self {
            pending,
            next_attempt_at: None,
            attempts: 0,
        }
    }

    /// 每帧调用: 满足条件时请求进入原生全屏.
    pub fn maybe_apply(&mut self, ctx: &egui::Context, window_visible: bool) {
        if !self.pending {
            return;
        }
        if !window_visible {
            // 窗口隐藏到托盘期间不能切换全屏, 等用户重新打开主界面再重新计时.
            self.next_attempt_at = None;
            return;
        }

        let now = Instant::now();
        // 收到请求后窗口的全屏标志会立刻变成 Some, 此时系统可能仍在做 space 动画.
        // 因此只在到达重试间隔边界时才判定结果, 避免把过渡中的状态当成成功.
        let next = *self
            .next_attempt_at
            .get_or_insert_with(|| now + FULLSCREEN_FIRST_DELAY);
        if now < next {
            ctx.request_repaint_after(next - now);
            return;
        }
        if ctx.input(|input| input.viewport().fullscreen.unwrap_or(false)) {
            self.pending = false;
            self.next_attempt_at = None;
            tracing::info!("native fullscreen restored");
            return;
        }
        if self.attempts >= FULLSCREEN_MAX_ATTEMPTS {
            self.pending = false;
            self.next_attempt_at = None;
            tracing::warn!(
                attempts = self.attempts,
                "giving up restoring native fullscreen, switch manually if needed"
            );
            return;
        }

        self.attempts += 1;
        self.next_attempt_at = Some(now + FULLSCREEN_RETRY_INTERVAL);
        tracing::info!(
            attempt = self.attempts,
            "requesting native fullscreen now that the window is visible"
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        ctx.request_repaint_after(FULLSCREEN_RETRY_INTERVAL);
    }
}

fn is_placeholder_position(pos: egui::Pos2) -> bool {
    !pos.x.is_finite() || !pos.y.is_finite() || pos.x <= -30_000.0 || pos.y <= -30_000.0
}

fn write_kv(path: &Path, kv: &HashMap<String, String>) {
    let Ok(file) = File::create(path) else {
        tracing::warn!("failed to rewrite persisted state {}", path.display());
        return;
    };
    let mut writer = BufWriter::new(file);
    let config = Default::default();
    if let Err(err) = ron::Options::default()
        .to_io_writer_pretty(&mut writer, kv, config)
        .and_then(|()| writer.flush().map_err(Into::into))
    {
        tracing::warn!(error = %err, "failed to serialize persisted state");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemoryStorage(HashMap<String, String>);

    impl Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }

        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }

        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }

        fn flush(&mut self) {}
    }

    fn invalid_settings() -> PersistedWindowSettings {
        PersistedWindowSettings {
            inner_position_pixels: Some(egui::pos2(-32000.0, -32000.0)),
            outer_position_pixels: Some(egui::pos2(-32000.0, -32000.0)),
            fullscreen: false,
            maximized: false,
            inner_size_points: Some(egui::vec2(0.0, 0.0)),
        }
    }

    fn valid_settings() -> PersistedWindowSettings {
        PersistedWindowSettings {
            inner_position_pixels: Some(egui::pos2(100.0, 100.0)),
            outer_position_pixels: Some(egui::pos2(100.0, 100.0)),
            fullscreen: false,
            maximized: false,
            inner_size_points: Some(DEFAULT_WINDOW_SIZE),
        }
    }

    #[test]
    fn rejects_hidden_window_placeholder() {
        assert!(!invalid_settings().is_valid());
        assert!(valid_settings().is_valid());
    }

    #[test]
    fn sanitize_removes_invalid_state_without_last_good() {
        let mut storage = MemoryStorage::default();
        eframe::set_value(&mut storage, WINDOW_KEY, &invalid_settings());

        assert!(sanitize_on_save(&mut storage, None).is_none());
        assert_eq!(storage.get_string(WINDOW_KEY), None);
    }

    #[test]
    fn sanitize_restores_last_good_state() {
        let mut storage = MemoryStorage::default();
        eframe::set_value(&mut storage, WINDOW_KEY, &invalid_settings());

        let restored = sanitize_on_save(&mut storage, Some(&valid_settings()));
        assert_eq!(
            restored.as_ref().map(PersistedWindowSettings::is_valid),
            Some(true)
        );
        let stored = eframe::get_value::<PersistedWindowSettings>(&storage, WINDOW_KEY);
        assert_eq!(
            stored.as_ref().map(PersistedWindowSettings::is_valid),
            Some(true)
        );
    }

    #[test]
    fn sanitize_file_keeps_other_keys_and_removes_bad_window() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-window-state-{}.ron",
            std::process::id()
        ));
        let mut kv = HashMap::new();
        kv.insert("egui".to_owned(), "(theme_preference:System)".to_owned());
        kv.insert(
            WINDOW_KEY.to_owned(),
            ron::ser::to_string(&invalid_settings()).unwrap(),
        );
        write_kv(&path, &kv);

        sanitize_file(&path);

        let restored: HashMap<String, String> =
            ron::de::from_reader(std::fs::File::open(&path).unwrap()).unwrap();
        assert!(restored.contains_key("egui"));
        assert!(!restored.contains_key(WINDOW_KEY));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn take_initial_fullscreen_clears_flag_once() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-fullscreen-state-{}.ron",
            std::process::id()
        ));
        let mut kv = HashMap::new();
        let mut settings = valid_settings();
        settings.fullscreen = true;
        kv.insert(WINDOW_KEY.to_owned(), ron::ser::to_string(&settings).unwrap());
        write_kv(&path, &kv);

        assert!(take_initial_fullscreen(&path));
        assert!(!take_initial_fullscreen(&path));

        let restored: HashMap<String, String> =
            ron::de::from_reader(std::fs::File::open(&path).unwrap()).unwrap();
        let settings: PersistedWindowSettings =
            ron::from_str(restored.get(WINDOW_KEY).unwrap()).unwrap();
        assert!(!settings.fullscreen);
        assert_eq!(settings.inner_size_points, Some(DEFAULT_WINDOW_SIZE));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn deferred_fullscreen_waits_until_window_is_visible() {
        let ctx = egui::Context::default();
        let mut deferred = DeferredFullscreen::new(true);

        deferred.maybe_apply(&ctx, false);
        assert!(deferred.pending);
        assert!(deferred.next_attempt_at.is_none());

        deferred.maybe_apply(&ctx, true);
        assert!(deferred.pending);
        assert!(deferred.next_attempt_at.is_some());
    }

    #[test]
    fn deferred_fullscreen_idle_without_persisted_flag() {
        let ctx = egui::Context::default();
        let mut deferred = DeferredFullscreen::new(false);

        deferred.maybe_apply(&ctx, true);
        assert!(!deferred.pending);
        assert!(deferred.next_attempt_at.is_none());
    }
}
