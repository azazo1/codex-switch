#[cfg(target_os = "macos")]
mod macos {
    use anyhow::{Context, bail};
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use std::{path::Path, process::Command};

    #[derive(Debug, Clone, Copy, Default)]
    enum ReopenState {
        #[default]
        Idle,
        WaitingForInactive,
        WaitingForActive,
    }

    #[derive(Debug, Default)]
    pub struct BackgroundReopenMonitor {
        state: ReopenState,
    }

    impl BackgroundReopenMonitor {
        pub fn mark_hidden(&mut self) {
            self.state = ReopenState::WaitingForInactive;
        }

        pub fn mark_shown(&mut self) {
            self.state = ReopenState::Idle;
        }

        pub fn should_show_hidden_window(&mut self) -> bool {
            let active = app_is_active();
            match self.state {
                ReopenState::Idle => false,
                ReopenState::WaitingForInactive => {
                    if !active {
                        self.state = ReopenState::WaitingForActive;
                    }
                    false
                }
                ReopenState::WaitingForActive => {
                    if active {
                        self.state = ReopenState::Idle;
                        true
                    } else {
                        false
                    }
                }
            }
        }
    }

    pub fn hide_from_dock() {
        if let Err(err) = set_activation_policy(NSApplicationActivationPolicy::Accessory, false) {
            tracing::warn!(error = %err, "failed to hide dock icon");
        }
    }

    pub fn show_in_dock() {
        if let Err(err) = set_activation_policy(NSApplicationActivationPolicy::Regular, true) {
            tracing::warn!(error = %err, "failed to show dock icon");
        }
    }

    pub fn app_is_active() -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        let app = NSApplication::sharedApplication(mtm);
        app.isActive()
    }

    fn set_activation_policy(
        policy: NSApplicationActivationPolicy,
        activate: bool,
    ) -> anyhow::Result<()> {
        let mtm = MainThreadMarker::new().context("appkit call must run on main thread")?;
        let app = NSApplication::sharedApplication(mtm);
        if !app.setActivationPolicy(policy) {
            bail!("appkit rejected activation policy change");
        }
        if activate {
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
        Ok(())
    }

    pub fn open_file_location(path: impl AsRef<Path>) -> anyhow::Result<()> {
        let status = Command::new("open")
            .arg("-R")
            .arg(path.as_ref())
            .status()
            .context("failed to open file location")?;
        if !status.success() {
            bail!("open returned {status}");
        }
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod macos {
    use anyhow::{Context, bail};
    use std::{path::Path, process::Command};

    #[derive(Debug, Default)]
    pub struct BackgroundReopenMonitor {
        _private: (),
    }

    impl BackgroundReopenMonitor {
        pub fn mark_hidden(&mut self) {}

        pub fn mark_shown(&mut self) {}

        pub fn should_show_hidden_window(&mut self) -> bool {
            false
        }
    }

    pub fn hide_from_dock() {}

    pub fn show_in_dock() {}

    pub fn open_file_location(path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        #[cfg(target_os = "windows")]
        let status = Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .status()
            .context("failed to open file location")?;
        #[cfg(not(target_os = "windows"))]
        let status = Command::new("xdg-open")
            .arg(path.parent().unwrap_or(path))
            .status()
            .context("failed to open file location")?;
        if !status.success() {
            bail!("file browser returned {status}");
        }
        Ok(())
    }
}

pub use macos::{BackgroundReopenMonitor, hide_from_dock, open_file_location, show_in_dock};
