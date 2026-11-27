//! Persistent main-window geometry and state (`~/.odg/window-state.json`).
//!
//! Records physical position `(x, y)`, inner size `(width, height)`, `maximized`,
//! and `fullscreen` across app restarts and tray hide/show cycles, while guarding
//! against Windows minimized bogus coordinates (`-32000, -32000`) and disconnected
//! external monitors.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{PhysicalPosition, PhysicalSize, WebviewWindow};

const STATE_FILE: &str = "window-state.json";
const MIN_VALID_WIDTH: u32 = 320;
const MIN_VALID_HEIGHT: u32 = 240;
const MIN_VISIBLE_OVERLAP: i64 = 64;
const SAVE_THROTTLE: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
    #[serde(default)]
    pub fullscreen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Manages in-memory and on-disk window state with throttled writes during live drag/resize
/// and immediate synchronous flushes on close/exit.
pub struct WindowStateManager {
    path: PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    state: Option<WindowState>,
    last_saved: Option<Instant>,
    dirty: bool,
}

impl WindowStateManager {
    pub fn new(home: &Path) -> Self {
        let path = home.join(STATE_FILE);
        let loaded = load_from_disk(&path);
        Self {
            path,
            inner: Mutex::new(Inner {
                state: loaded,
                last_saved: None,
                dirty: false,
            }),
        }
    }

    /// Restore the saved geometry/state onto `window` before showing it.
    pub fn restore(&self, window: &WebviewWindow) {
        let Some(saved) = self.inner.lock().ok().and_then(|g| g.state.clone()) else {
            return;
        };
        if saved.width < MIN_VALID_WIDTH || saved.height < MIN_VALID_HEIGHT {
            return;
        }

        let monitors: Vec<MonitorRect> = window
            .available_monitors()
            .unwrap_or_default()
            .into_iter()
            .map(|m| {
                let pos = m.position();
                let size = m.size();
                MonitorRect {
                    x: pos.x,
                    y: pos.y,
                    width: size.width,
                    height: size.height,
                }
            })
            .collect();

        // Always restore the normal size first so unmaximizing later lands on a sensible size.
        let _ = window.set_size(PhysicalSize::new(saved.width, saved.height));

        if monitors.is_empty() || is_rect_visible_on_monitors(&saved, &monitors) {
            let _ = window.set_position(PhysicalPosition::new(saved.x, saved.y));
        } else {
            let _ = window.center();
        }

        if saved.fullscreen {
            let _ = window.set_fullscreen(true);
        } else if saved.maximized {
            let _ = window.maximize();
        }
    }

    /// Capture the current geometry/state of `window` (if visible and not minimized).
    /// When `flush_now` is true, writes immediately to disk; otherwise throttles disk I/O.
    pub fn capture(&self, window: &WebviewWindow, flush_now: bool) {
        // Ignore hidden or minimized windows (Windows reports -32000,-32000 and 0x0 when minimized).
        if window.is_visible().ok() == Some(false) || window.is_minimized().ok() == Some(true) {
            if flush_now {
                self.flush();
            }
            return;
        }

        let fullscreen = window.is_fullscreen().unwrap_or(false);
        let maximized = window.is_maximized().unwrap_or(false);
        let pos = window.outer_position().ok();
        let size = window.inner_size().ok();

        let to_write = {
            let Ok(mut guard) = self.inner.lock() else {
                return;
            };
            let next = merge_window_sample(
                guard.state.as_ref(),
                pos.map(|p| (p.x, p.y)),
                size.map(|s| (s.width, s.height)),
                maximized,
                fullscreen,
            );
            let Some(next) = next else {
                return;
            };
            if guard.state.as_ref() != Some(&next) {
                guard.state = Some(next);
                guard.dirty = true;
            }
            let should_write =
                guard.dirty && (flush_now || guard.last_saved.map(|t| t.elapsed() >= SAVE_THROTTLE).unwrap_or(true));
            if should_write {
                guard.dirty = false;
                guard.last_saved = Some(Instant::now());
                guard.state.clone()
            } else {
                None
            }
        };

        if let Some(state) = to_write {
            let _ = save_to_disk(&self.path, &state);
        }
    }

    /// Persist any pending dirty state immediately (used on exit).
    pub fn flush(&self) {
        let to_write = {
            let Ok(mut guard) = self.inner.lock() else {
                return;
            };
            if guard.dirty {
                guard.dirty = false;
                guard.last_saved = Some(Instant::now());
                guard.state.clone()
            } else {
                None
            }
        };
        if let Some(state) = to_write {
            let _ = save_to_disk(&self.path, &state);
        }
    }
}

/// Merge a newly sampled window state with the previous normal geometry.
/// When the window is maximized or fullscreen, preserves the previous normal `(x, y, width, height)`
/// so unmaximizing restores the pre-maximized bounds.
pub fn merge_window_sample(
    prev: Option<&WindowState>,
    pos: Option<(i32, i32)>,
    size: Option<(u32, u32)>,
    maximized: bool,
    fullscreen: bool,
) -> Option<WindowState> {
    if maximized || fullscreen {
        if let Some(prev) = prev {
            return Some(WindowState {
                maximized,
                fullscreen,
                ..prev.clone()
            });
        }
    }
    let (x, y) = pos?;
    let (width, height) = size?;
    if width < MIN_VALID_WIDTH || height < MIN_VALID_HEIGHT {
        return prev.cloned();
    }
    Some(WindowState {
        x,
        y,
        width,
        height,
        maximized,
        fullscreen,
    })
}

/// Returns true if the saved window rectangle overlaps at least one monitor by at least
/// `MIN_VISIBLE_OVERLAP × MIN_VISIBLE_OVERLAP` physical pixels.
pub fn is_rect_visible_on_monitors(state: &WindowState, monitors: &[MonitorRect]) -> bool {
    let win_left = i64::from(state.x);
    let win_top = i64::from(state.y);
    let win_right = win_left + i64::from(state.width);
    let win_bottom = win_top + i64::from(state.height);

    monitors.iter().any(|m| {
        let mon_left = i64::from(m.x);
        let mon_top = i64::from(m.y);
        let mon_right = mon_left + i64::from(m.width);
        let mon_bottom = mon_top + i64::from(m.height);

        let overlap_w = (win_right.min(mon_right) - win_left.max(mon_left)).max(0);
        let overlap_h = (win_bottom.min(mon_bottom) - win_top.max(mon_top)).max(0);
        overlap_w >= MIN_VISIBLE_OVERLAP && overlap_h >= MIN_VISIBLE_OVERLAP
    })
}

fn load_from_disk(path: &Path) -> Option<WindowState> {
    let raw = std::fs::read_to_string(path).ok()?;
    let state: WindowState = serde_json::from_str(&raw).ok()?;
    if state.width < MIN_VALID_WIDTH || state.height < MIN_VALID_HEIGHT {
        return None;
    }
    Some(state)
}

fn save_to_disk(path: &Path, state: &WindowState) -> Result<(), String> {
    let json = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    odg_core::fsutil::write_atomically(path, json.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_window_state_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let sample = WindowState {
            x: 120,
            y: 80,
            width: 1440,
            height: 900,
            maximized: true,
            fullscreen: false,
        };
        save_to_disk(&path, &sample).unwrap();
        assert_eq!(load_from_disk(&path), Some(sample));
    }

    #[test]
    fn preserves_normal_bounds_when_maximized_or_fullscreen_and_ignores_tiny_sizes() {
        let normal = WindowState {
            x: 100,
            y: 60,
            width: 1280,
            height: 800,
            maximized: false,
            fullscreen: false,
        };
        let maxed = merge_window_sample(Some(&normal), Some((0, 0)), Some((2560, 1440)), true, false).unwrap();
        assert_eq!(maxed.x, 100);
        assert_eq!(maxed.y, 60);
        assert_eq!(maxed.width, 1280);
        assert_eq!(maxed.height, 800);
        assert!(maxed.maximized);
        assert!(!maxed.fullscreen);

        // Windows minimized bogus coordinates / 0x0 size must not clobber normal bounds.
        let ignored = merge_window_sample(Some(&normal), Some((-32000, -32000)), Some((0, 0)), false, false).unwrap();
        assert_eq!(ignored, normal);
    }

    #[test]
    fn detects_offscreen_window_after_monitor_disconnect() {
        let primary = [MonitorRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }];
        let visible = WindowState {
            x: 200,
            y: 120,
            width: 1200,
            height: 800,
            maximized: false,
            fullscreen: false,
        };
        assert!(is_rect_visible_on_monitors(&visible, &primary));

        let disconnected = WindowState {
            x: 2200,
            y: 100,
            width: 1200,
            height: 800,
            maximized: false,
            fullscreen: false,
        };
        assert!(!is_rect_visible_on_monitors(&disconnected, &primary));
    }
}
