#[cfg(target_os = "macos")]
mod macos {
    use anyhow::{Context, bail};
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

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
}

#[cfg(not(target_os = "macos"))]
mod macos {
    pub fn hide_from_dock() {}

    pub fn show_in_dock() {}
}

pub use macos::{hide_from_dock, show_in_dock};
