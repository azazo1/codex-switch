use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, BufWriter, Write as _},
    path::Path,
};

use eframe::{Storage, egui};
use serde::{Deserialize, Serialize};

pub const DEFAULT_WINDOW_SIZE: egui::Vec2 = egui::vec2(1100.0, 720.0);

const WINDOW_KEY: &str = "window";

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
            inner_size_points: Some(egui::vec2(1100.0, 720.0)),
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
        assert_eq!(restored.as_ref().map(PersistedWindowSettings::is_valid), Some(true));
        let stored = eframe::get_value::<PersistedWindowSettings>(&storage, WINDOW_KEY);
        assert_eq!(stored.as_ref().map(PersistedWindowSettings::is_valid), Some(true));
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
}
