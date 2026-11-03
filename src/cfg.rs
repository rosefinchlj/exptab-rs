//! Tiny ini-style configuration: `%LOCALAPPDATA%\exptab\config.ini`.
//!
//! Deliberately no serde/ini crate: one dependency (`windows`) keeps the build
//! fast and the binary small.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    /// Master switch; also flippable at runtime through the IPC socket.
    pub enabled: bool,
    /// Only merge windows that still have a single tab (a window the user
    /// opened with several tabs is left alone).
    pub merge_single_tab_only: bool,
    /// Holding Ctrl+Shift while triggering the window bypasses the merge.
    pub bypass_ctrl_shift: bool,
    /// Detection poll interval, used only as a *safety net*: the detection
    /// thread normally sleeps in `WaitForSingleObject` on a WinEvent hook and is
    /// woken the moment Explorer creates/shows a window or the foreground
    /// changes, so this timeout only bounds the case of a missed event. 20ms
    /// costs ~50 wakes/s of an otherwise idle thread (well under 0.1% of a
    /// core) and still recovers from a lost event long before a user would
    /// notice.
    pub detect_poll_ms: u64,
    /// Detection interval used while there is no Explorer window on screen,
    /// i.e. nothing a new window could be merged into. A merge also needs a
    /// visible main window, so reacting fast while idle only costs CPU. 100ms
    /// keeps the "first window becomes the main window" delay imperceptible
    /// while making the daemon effectively free when Explorer is closed.
    pub detect_poll_idle_ms: u64,
    /// How long to wait for the new window to appear in IShellWindows
    /// (registration; measured 60ms-2s depending on Explorer load).
    pub max_wait_ms: u64,
    /// Append one CSV row per merge attempt to `metrics.csv`.
    pub metrics: bool,
    /// Pin the window that receives merged tabs; 0 = pick automatically.
    pub main_hwnd: isize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            merge_single_tab_only: true,
            bypass_ctrl_shift: true,
            detect_poll_ms: 20,
            detect_poll_idle_ms: 100,
            max_wait_ms: 3000,
            metrics: true,
            main_hwnd: 0,
        }
    }
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    let dir = PathBuf::from(base).join("exptab");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.ini")
}

pub fn metrics_path() -> PathBuf {
    data_dir().join("metrics.csv")
}

fn parse_bool(v: &str, default: bool) -> bool {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

fn parse_hwnd(v: &str) -> Option<isize> {
    let v = v.trim();
    match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        Some(hex) => isize::from_str_radix(hex, 16).ok(),
        None => v.parse().ok(),
    }
}

impl Config {
    pub fn load() -> Self {
        let mut cfg = Self::default();
        let Ok(text) = std::fs::read_to_string(config_path()) else {
            let _ = cfg.save();
            return cfg;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "enabled" => cfg.enabled = parse_bool(value, cfg.enabled),
                "merge_single_tab_only" => {
                    cfg.merge_single_tab_only = parse_bool(value, cfg.merge_single_tab_only)
                }
                "bypass_ctrl_shift" => {
                    cfg.bypass_ctrl_shift = parse_bool(value, cfg.bypass_ctrl_shift)
                }
                "detect_poll_ms" => {
                    cfg.detect_poll_ms = value.parse().unwrap_or(cfg.detect_poll_ms).clamp(1, 50)
                }
                "detect_poll_idle_ms" => {
                    cfg.detect_poll_idle_ms =
                        value.parse().unwrap_or(cfg.detect_poll_idle_ms).clamp(1, 1000)
                }
                "max_wait_ms" => {
                    cfg.max_wait_ms = value.parse().unwrap_or(cfg.max_wait_ms).clamp(500, 15000)
                }
                "metrics" => cfg.metrics = parse_bool(value, cfg.metrics),
                "main_hwnd" => cfg.main_hwnd = parse_hwnd(value).unwrap_or(cfg.main_hwnd),
                _ => {}
            }
        }
        cfg
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::write(config_path(), self.to_ini())
    }

    fn to_ini(&self) -> String {
        format!(
            "# exptab configuration (edit while the daemon is stopped, or use\n\
             # `exptab on` / `exptab off` for the master switch)\n\
             enabled={}\n\
             merge_single_tab_only={}\n\
             bypass_ctrl_shift={}\n\
             detect_poll_ms={}\n\
             detect_poll_idle_ms={}\n\
             max_wait_ms={}\n\
             metrics={}\n\
             main_hwnd=0x{:x}\n",
            self.enabled,
            self.merge_single_tab_only,
            self.bypass_ctrl_shift,
            self.detect_poll_ms,
            self.detect_poll_idle_ms,
            self.max_wait_ms,
            self.metrics,
            self.main_hwnd
        )
    }
}
