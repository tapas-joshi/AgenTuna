//! Durable window layout model and display-clamping logic.

use tauri::Manager;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Saved desktop window geometry and persistent shell sizing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct WindowLayout {
    /// Horizontal physical-pixel position.
    pub x: i32,
    /// Vertical physical-pixel position.
    pub y: i32,
    /// Physical-pixel width.
    pub width: u32,
    /// Physical-pixel height.
    pub height: u32,
    /// Monitor identifier, when available.
    pub monitor_id: Option<String>,
    /// Whether the window was maximized.
    pub maximized: bool,
    /// Sidebar width in logical pixels.
    pub sidebar_width: u32,
    /// Persisted data-table column widths in logical pixels.
    pub column_widths: BTreeMap<String, u32>,
}

/// Geometry of a currently available display in physical pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayBounds {
    /// Stable display identifier.
    pub id: String,
    /// Horizontal origin.
    pub x: i32,
    /// Vertical origin.
    pub y: i32,
    /// Display width.
    pub width: u32,
    /// Display height.
    pub height: u32,
    /// Whether this is the primary display.
    pub primary: bool,
    /// Display scale factor in percent (100 = 1x, 200 = Retina), so logical window minimums
    /// can be compared with the physical pixel sizes the layout stores.
    pub scale_percent: u32,
}

/// Smallest window width in logical pixels (matches `minWidth` in `tauri.conf.json`).
const LOGICAL_MIN_WIDTH: u32 = 960;
/// Smallest window height in logical pixels (matches `minHeight` in `tauri.conf.json`).
const LOGICAL_MIN_HEIGHT: u32 = 640;
/// Default window width in logical pixels on a first run.
const LOGICAL_DEFAULT_WIDTH: u32 = 1440;
/// Default window height in logical pixels on a first run.
const LOGICAL_DEFAULT_HEIGHT: u32 = 900;

/// Physical pixels of a logical minimum on `display`.
fn minimum(logical: u32, display: &DisplayBounds) -> u32 {
    logical
        .saturating_mul(display.scale_percent)
        .saturating_div(100)
}

/// 92% of a display dimension: leaves room for the menu bar and Dock.
fn fit(physical: u32) -> u32 {
    physical.saturating_mul(92).saturating_div(100)
}

/// Returns a safe layout, clamped to a connected display.
///
/// Invalid sizes or an empty display list return `None`; disconnected monitors
/// are centered on the primary display (or first display).
#[must_use]
pub fn clamp_layout(saved: &WindowLayout, displays: &[DisplayBounds]) -> Option<WindowLayout> {
    let primary = displays
        .iter()
        .find(|display| display.primary)
        .or_else(|| displays.first())?;
    let monitor = displays
        .iter()
        .find(|display| saved.monitor_id.as_deref() == Some(display.id.as_str()));
    let display = monitor.unwrap_or(primary);
    if saved.width < minimum(LOGICAL_MIN_WIDTH, display)
        || saved.height < minimum(LOGICAL_MIN_HEIGHT, display)
        || saved.sidebar_width < 120
        || saved.sidebar_width > 600
    {
        return Some(default_layout(display));
    }
    let width = saved
        .width
        .min(display.width)
        .max(minimum(LOGICAL_MIN_WIDTH, display).min(display.width));
    let height = saved
        .height
        .min(display.height)
        .max(minimum(LOGICAL_MIN_HEIGHT, display).min(display.height));
    let x = saved.x.max(display.x).min(
        display
            .x
            .saturating_add(display.width.saturating_sub(width) as i32),
    );
    let y = saved.y.max(display.y).min(
        display
            .y
            .saturating_add(display.height.saturating_sub(height) as i32),
    );
    Some(WindowLayout {
        x: if monitor.is_some() {
            x
        } else {
            display
                .x
                .saturating_add(display.width.saturating_sub(width) as i32 / 2)
        },
        y: if monitor.is_some() {
            y
        } else {
            display
                .y
                .saturating_add(display.height.saturating_sub(height) as i32 / 2)
        },
        width,
        height,
        monitor_id: Some(display.id.clone()),
        maximized: saved.maximized,
        sidebar_width: saved.sidebar_width,
        column_widths: saved
            .column_widths
            .iter()
            .map(|(name, width)| (name.clone(), (*width).clamp(40, 1000)))
            .collect(),
    })
}

/// Converts a monitor scale factor (1.0, 2.0, …) to whole percent, bounded to 100–400.
fn scale_percent(factor: f64) -> u32 {
    let percent = (factor * 100.0).round();
    if percent.is_finite() && (100.0..=400.0).contains(&percent) {
        // The range check above makes the conversion exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let whole = percent as u32;
        whole
    } else {
        100
    }
}

fn default_layout(display: &DisplayBounds) -> WindowLayout {
    let width = LOGICAL_DEFAULT_WIDTH
        .saturating_mul(display.scale_percent)
        .saturating_div(100)
        .min(fit(display.width))
        .max(minimum(LOGICAL_MIN_WIDTH, display).min(display.width));
    let height = LOGICAL_DEFAULT_HEIGHT
        .saturating_mul(display.scale_percent)
        .saturating_div(100)
        .min(fit(display.height))
        .max(minimum(LOGICAL_MIN_HEIGHT, display).min(display.height));
    WindowLayout {
        x: display
            .x
            .saturating_add(display.width.saturating_sub(width) as i32 / 2),
        y: display
            .y
            .saturating_add(display.height.saturating_sub(height) as i32 / 2),
        width,
        height,
        monitor_id: Some(display.id.clone()),
        maximized: false,
        sidebar_width: 240,
        column_widths: BTreeMap::new(),
    }
}

/// Restores the persisted geometry during Tauri setup, before the webview is shown.
pub fn restore_window(window: &tauri::WebviewWindow, state: &super::state::AppState) {
    let monitors = match window.available_monitors() {
        Ok(monitors) => monitors,
        Err(error) => {
            tracing::warn!(%error, "could not enumerate displays for window restore");
            return;
        }
    };
    let primary = window.primary_monitor().ok().flatten();
    let displays = monitors
        .iter()
        .enumerate()
        .map(|(index, monitor)| DisplayBounds {
            id: monitor
                .name()
                .map(|name| name.to_string())
                .unwrap_or_else(|| format!("display-{index}")),
            x: monitor.position().x,
            y: monitor.position().y,
            width: monitor.size().width,
            height: monitor.size().height,
            scale_percent: scale_percent(monitor.scale_factor()),
            primary: primary.as_ref().is_some_and(|screen| {
                screen.position().x == monitor.position().x
                    && screen.position().y == monitor.position().y
                    && screen.size().width == monitor.size().width
                    && screen.size().height == monitor.size().height
            }),
        })
        .collect::<Vec<_>>();
    if displays.is_empty() {
        return;
    }
    let stored = state.database().ok().and_then(|database| {
        let document = at_db::settings::export(database.connection()).ok()?;
        let raw = document.values.get("window_state")?;
        let encoded: String = serde_json::from_str(raw.get()).ok()?;
        serde_json::from_str::<WindowLayout>(&encoded).ok()
    });
    let fallback = WindowLayout {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
        monitor_id: None,
        maximized: false,
        sidebar_width: 240,
        column_widths: BTreeMap::new(),
    };
    if stored.is_none() {
        tracing::warn!("saved window layout is missing or invalid; using default geometry");
    }
    let Some(layout) = clamp_layout(stored.as_ref().unwrap_or(&fallback), &displays) else {
        return;
    };
    let _ = window.set_size(tauri::PhysicalSize::new(layout.width, layout.height));
    let _ = window.set_position(tauri::PhysicalPosition::new(layout.x, layout.y));
    if layout.maximized {
        let _ = window.maximize();
    } else {
        let _ = window.unmaximize();
    }
}
/// Persists the current native geometry after the window is moved or resized.
pub fn persist_window(window: &tauri::WebviewWindow, state: &super::state::AppState) {
    let Ok(position) = window.outer_position() else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let monitor_id = window
        .current_monitor()
        .ok()
        .flatten()
        .and_then(|monitor| monitor.name().map(|name| name.to_string()));
    let previous = state.database().ok().and_then(|database| {
        let document = at_db::settings::export(database.connection()).ok()?;
        let raw = document.values.get("window_state")?;
        let encoded: String = serde_json::from_str(raw.get()).ok()?;
        serde_json::from_str::<WindowLayout>(&encoded).ok()
    });
    let layout = WindowLayout {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
        monitor_id,
        maximized: window.is_maximized().unwrap_or(false),
        sidebar_width: previous.as_ref().map_or(240, |saved| saved.sidebar_width),
        column_widths: previous.map_or_else(BTreeMap::new, |saved| saved.column_widths),
    };
    let Ok(serialized) = serde_json::to_string(&layout) else {
        return;
    };
    let Ok(mut database) = state.database() else {
        return;
    };
    if let Err(error) = at_db::settings::set_value(
        database.connection_mut(),
        &at_db::settings::SettingsKey::WindowState,
        &serde_json::Value::String(serialized).to_string(),
        at_db::ops::OperationActor::Ui,
    ) {
        tracing::warn!(%error, "could not persist window geometry");
    }
}

#[cfg(test)]
mod tests {
    use super::{DisplayBounds, WindowLayout, clamp_layout};
    use std::collections::BTreeMap;

    fn display(id: &str, primary: bool) -> DisplayBounds {
        DisplayBounds {
            id: id.to_owned(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            primary,
            scale_percent: 100,
        }
    }

    #[test]
    fn a_tiny_saved_size_on_a_retina_display_resets_to_a_usable_default() {
        // 1280x800 physical pixels is only 640x400 logical points on a 2x display: below the
        // 960x640 window minimum, so it must not be restored.
        let retina = DisplayBounds {
            id: "retina".to_owned(),
            x: 0,
            y: 0,
            width: 3024,
            height: 1964,
            primary: true,
            scale_percent: 200,
        };
        let saved = WindowLayout {
            x: 100,
            y: 100,
            width: 1280,
            height: 800,
            monitor_id: Some("retina".to_owned()),
            maximized: false,
            sidebar_width: 250,
            column_widths: BTreeMap::new(),
        };
        let clamped = clamp_layout(&saved, &[retina]).expect("display exists");
        // 1440x900 logical = 2880x1800 physical, capped at 92% of the display width.
        assert_eq!((clamped.width, clamped.height), (2782, 1800));
    }

    #[test]
    fn the_default_never_exceeds_a_small_display_but_keeps_the_logical_minimum() {
        let small = DisplayBounds {
            id: "small".to_owned(),
            x: 0,
            y: 0,
            width: 1366,
            height: 768,
            primary: true,
            scale_percent: 100,
        };
        let saved = WindowLayout {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            monitor_id: None,
            maximized: false,
            sidebar_width: 250,
            column_widths: BTreeMap::new(),
        };
        let clamped = clamp_layout(&saved, &[small]).expect("display exists");
        assert_eq!((clamped.width, clamped.height), (1256, 706));
    }

    #[test]
    fn clamps_geometry_and_recenters_disconnected_monitor() {
        let saved = WindowLayout {
            x: 9000,
            y: -500,
            width: 1280,
            height: 800,
            monitor_id: Some("gone".to_owned()),
            maximized: true,
            sidebar_width: 250,
            column_widths: BTreeMap::new(),
        };
        let clamped = clamp_layout(&saved, &[display("primary", true)]).expect("display exists");
        assert_eq!(
            (clamped.x, clamped.y, clamped.width, clamped.height),
            (320, 140, 1280, 800)
        );
        assert!(clamped.maximized);
    }

    #[test]
    fn invalid_geometry_falls_back_to_default_layout() {
        let saved = WindowLayout {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            monitor_id: None,
            maximized: true,
            sidebar_width: 250,
            column_widths: BTreeMap::new(),
        };
        let clamped = clamp_layout(&saved, &[display("primary", true)]).expect("display exists");
        assert_eq!(
            (clamped.x, clamped.y, clamped.width, clamped.height),
            (240, 90, 1440, 900)
        );
        assert!(!clamped.maximized);
    }
    #[test]
    fn layouts_on_displays_smaller_than_the_logical_minimum_are_preserved() {
        let small = DisplayBounds {
            id: "small".to_owned(),
            x: 0,
            y: 0,
            width: 800,
            height: 600,
            primary: true,
            scale_percent: 100,
        };
        let saved = WindowLayout {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            monitor_id: None,
            maximized: false,
            sidebar_width: 250,
            column_widths: BTreeMap::new(),
        };
        let clamped = clamp_layout(&saved, &[small]).expect("display exists");
        assert_eq!((clamped.width, clamped.height), (800, 600));
    }

    #[test]
    fn clamps_geometry_on_a_display_with_a_negative_origin() {
        let left = DisplayBounds {
            id: "left".to_owned(),
            x: -1920,
            y: -1080,
            width: 1920,
            height: 1080,
            primary: false,
            scale_percent: 100,
        };
        let saved = WindowLayout {
            x: -3000,
            y: -2000,
            width: 1280,
            height: 800,
            monitor_id: Some("left".to_owned()),
            maximized: false,
            sidebar_width: 250,
            column_widths: BTreeMap::new(),
        };
        let clamped =
            clamp_layout(&saved, &[display("primary", true), left]).expect("display exists");
        assert_eq!((clamped.x, clamped.y), (-1920, -1080));
    }

    #[test]
    fn validates_saved_size_using_the_saved_monitors_scale() {
        let mut primary = display("primary", true);
        primary.width = 3840;
        primary.height = 2160;
        primary.scale_percent = 200;
        let saved = WindowLayout {
            x: 100,
            y: 100,
            width: 1200,
            height: 800,
            monitor_id: Some("one-x".to_owned()),
            maximized: false,
            sidebar_width: 250,
            column_widths: BTreeMap::new(),
        };
        let clamped =
            clamp_layout(&saved, &[primary, display("one-x", false)]).expect("display exists");
        assert_eq!(clamped.monitor_id.as_deref(), Some("one-x"));
        assert_eq!((clamped.width, clamped.height), (1200, 800));
    }

    #[test]
    fn clamp_layout_does_not_panic_for_extreme_geometry() {
        let origins = [i32::MIN, -1920, 0, i32::MAX];
        let dimensions = [0, 1, 600, 1920, i32::MAX as u32, u32::MAX];
        for x in origins {
            for y in origins {
                for width in dimensions {
                    for height in dimensions {
                        let screen = DisplayBounds {
                            id: "extreme".to_owned(),
                            x,
                            y,
                            width,
                            height,
                            primary: true,
                            scale_percent: 400,
                        };
                        let saved = WindowLayout {
                            x,
                            y,
                            width,
                            height,
                            monitor_id: Some("extreme".to_owned()),
                            maximized: false,
                            sidebar_width: 250,
                            column_widths: BTreeMap::new(),
                        };
                        assert!(
                            std::panic::catch_unwind(|| clamp_layout(&saved, &[screen])).is_ok(),
                            "clamp panicked for origin ({x}, {y}) and size ({width}, {height})"
                        );
                    }
                }
            }
        }
    }
}
/// Loads the saved window layout from the settings database.
#[tauri::command]
pub fn window_state_load(
    state: tauri::State<'_, super::state::AppState>,
) -> Result<Option<WindowLayout>, super::commands::CommandError> {
    let database = state
        .database()
        .map_err(super::commands::CommandError::unavailable)?;
    let document = at_db::settings::export(database.connection())
        .map_err(|error| super::commands::CommandError::internal(error.to_string()))?;
    let Some(raw) = document.values.get("window_state") else {
        return Ok(None);
    };
    let encoded: String = serde_json::from_str(raw.get())
        .map_err(|_| super::commands::CommandError::internal("saved window layout is invalid"))?;
    if encoded.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&encoded)
        .map(Some)
        .map_err(|_| super::commands::CommandError::internal("saved window layout is invalid"))
}

/// Saves a window layout as one validated settings value.
#[tauri::command]
pub fn window_state_save(
    state: tauri::State<'_, super::state::AppState>,
    layout: WindowLayout,
) -> Result<(), super::commands::CommandError> {
    if !(640..=16_384).contains(&layout.width)
        || !(480..=16_384).contains(&layout.height)
        || !(120..=600).contains(&layout.sidebar_width)
        || layout.column_widths.len() > 128
    {
        return Err(super::commands::CommandError::invalid_argument(
            "layout",
            "window layout is out of range",
        ));
    }
    if layout
        .column_widths
        .iter()
        .any(|(key, width)| key.is_empty() || key.len() > 128 || !(40..=1000).contains(width))
    {
        return Err(super::commands::CommandError::invalid_argument(
            "layout",
            "column widths are out of range",
        ));
    }
    let serialized = serde_json::to_string(&layout)
        .map_err(|_| super::commands::CommandError::internal("could not encode window layout"))?;
    let mut database = state
        .database()
        .map_err(super::commands::CommandError::unavailable)?;
    at_db::settings::set_value(
        database.connection_mut(),
        &at_db::settings::SettingsKey::WindowState,
        &serde_json::Value::String(serialized).to_string(),
        at_db::ops::OperationActor::Ui,
    )
    .map_err(|_| super::commands::CommandError::internal("could not save window layout"))
}

/// Clears the saved layout and immediately restores defaults on the main window.
#[tauri::command]
pub fn window_state_reset(
    app: tauri::AppHandle,
    state: tauri::State<'_, super::state::AppState>,
) -> Result<(), super::commands::CommandError> {
    let mut database = state
        .database()
        .map_err(super::commands::CommandError::unavailable)?;
    at_db::settings::set_value(
        database.connection_mut(),
        &at_db::settings::SettingsKey::WindowState,
        "\"\"",
        at_db::ops::OperationActor::Ui,
    )
    .map_err(|_| super::commands::CommandError::internal("could not reset window layout"))?;
    drop(database);
    if let Some(window) = app.get_webview_window("main") {
        restore_window(&window, &state);
    }
    Ok(())
}

#[cfg(test)]
mod persistence_tests {
    use super::WindowLayout;
    use std::collections::BTreeMap;

    #[test]
    fn settings_round_trip_layout_in_a_temporary_database() {
        let dir = tempfile::tempdir().expect("temporary db directory");
        let mut database = at_db::Database::open_file(dir.path().join("app.sqlite"))
            .expect("temporary database opens");
        let layout = WindowLayout {
            x: 12,
            y: 34,
            width: 1280,
            height: 800,
            monitor_id: Some("test-display".to_owned()),
            maximized: true,
            sidebar_width: 260,
            column_widths: BTreeMap::from([("skills.name".to_owned(), 320)]),
        };
        let json = serde_json::to_string(&layout).expect("layout serializes");
        at_db::settings::set_value(
            database.connection_mut(),
            &at_db::settings::SettingsKey::WindowState,
            &serde_json::Value::String(json).to_string(),
            at_db::ops::OperationActor::Ui,
        )
        .expect("window state writes");
        let document = at_db::settings::export(database.connection()).expect("settings export");
        let raw = document
            .values
            .get("window_state")
            .expect("saved state exists");
        let stored: String = serde_json::from_str(raw.get()).expect("stored string decodes");
        let restored: WindowLayout = serde_json::from_str(&stored).expect("layout round trips");
        assert_eq!(restored, layout);
    }
}
