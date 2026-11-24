//! The desktop's light/dark preference, read from the XDG desktop portal and kept current.
//!
//! winit doesn't report the system theme on Linux, so without this egui's "follow the system"
//! always falls back to dark.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use eframe::egui::{self, Theme};
use zbus::zvariant::{OwnedValue, Value};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";
const NAMESPACE: &str = "org.freedesktop.appearance";
const KEY: &str = "color-scheme";

/// The latest preference the portal reported. `None` until it answers, when there is no
/// portal, or when the desktop has no preference.
#[derive(Clone, Default)]
pub struct SystemTheme(Arc<Shared>);

#[derive(Default)]
struct Shared {
    theme: Mutex<Option<Theme>>,
    /// Set once the window exists, so changes can wake the UI.
    ctx: OnceLock<egui::Context>,
}

impl SystemTheme {
    /// Starts reading on a background thread. Start it early: the answer usually arrives
    /// before the first frame, so the window doesn't open in the wrong theme.
    pub fn watch() -> Self {
        let this = Self::default();
        let shared = Arc::clone(&this.0);
        let spawned = std::thread::Builder::new()
            .name("system-theme".into())
            .spawn(move || {
                if let Err(e) = run(&shared) {
                    tracing::info!("couldn't read the system theme from the desktop portal: {e}");
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("couldn't start watching the system theme: {e}");
        }
        this
    }

    /// Lets later changes wake the UI. An answer that came earlier is picked up by the first frame.
    pub fn set_context(&self, ctx: egui::Context) {
        let _ = self.0.ctx.set(ctx);
    }

    pub fn get(&self) -> Option<Theme> {
        *self.0.theme.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Shared {
    fn set(&self, theme: Option<Theme>) {
        *self.theme.lock().unwrap_or_else(PoisonError::into_inner) = theme;
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
        }
    }
}

fn run(shared: &Shared) -> zbus::Result<()> {
    let conn = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL, PORTAL_PATH, SETTINGS)?;
    // Subscribe before reading so a change in between isn't missed.
    let changes = proxy.receive_signal_with_args("SettingChanged", &[(0, NAMESPACE), (1, KEY)])?;
    let value: OwnedValue = proxy
        .call("ReadOne", &(NAMESPACE, KEY))
        // Portals older than version 2 only have Read.
        .or_else(|_| proxy.call("Read", &(NAMESPACE, KEY)))?;
    shared.set(color_scheme(&value));
    for message in changes {
        let (_, _, value): (String, String, OwnedValue) = message.body().deserialize()?;
        shared.set(color_scheme(&value));
    }
    Ok(())
}

/// `color-scheme` is 1 for dark, 2 for light and 0 for no preference.
/// The old Read method wraps it in an extra variant.
fn color_scheme(value: &Value) -> Option<Theme> {
    match value {
        Value::Value(inner) => color_scheme(inner),
        Value::U32(1) => Some(Theme::Dark),
        Value::U32(2) => Some(Theme::Light),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_scheme_values() {
        assert_eq!(color_scheme(&Value::U32(0)), None);
        assert_eq!(color_scheme(&Value::U32(1)), Some(Theme::Dark));
        assert_eq!(color_scheme(&Value::U32(2)), Some(Theme::Light));
        assert_eq!(color_scheme(&Value::U32(3)), None);
        assert_eq!(color_scheme(&Value::from("dark")), None);
    }

    #[test]
    fn old_read_method_is_unwrapped() {
        let wrapped = Value::Value(Box::new(Value::U32(2)));
        assert_eq!(color_scheme(&wrapped), Some(Theme::Light));
    }
}
