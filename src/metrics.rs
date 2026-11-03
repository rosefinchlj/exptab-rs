//! Append-only CSV metrics: one row per merge attempt, QPC-derived durations.
//!
//! `%LOCALAPPDATA%\exptab\metrics.csv`, key=value columns so new columns can be
//! added without breaking old files.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

static WRITER: Mutex<Option<std::fs::File>> = Mutex::new(None);

pub const HEADER: &str = "ts_unix_ms,source_hwnd,source_pid,result,detect_to_hide_us,\
hide_us,source_tab_us,source_registered_us,new_tab_us,dest_registered_us,navigate_us,\
source_closed_us,total_us,perceived_us,visible_at_detect,source_tabs,main_hwnd";

pub fn init(enabled: bool) {
    if !enabled {
        return;
    }
    let path = crate::cfg::metrics_path();
    let new_file = !path.exists();
    let opened = OpenOptions::new().create(true).append(true).open(&path);
    match opened {
        Ok(mut f) => {
            if new_file {
                let _ = writeln!(f, "{HEADER}");
            }
            *WRITER.lock().unwrap() = Some(f);
        }
        Err(e) => eprintln!("exptab: metrics disabled, cannot open {}: {e}", path.display()),
    }
}

pub fn emit(line: &str) {
    if let Some(f) = WRITER.lock().unwrap().as_mut() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = writeln!(f, "{ts},{line}");
        let _ = f.flush();
    }
}

/// A merge attempt row; durations are microseconds.
#[derive(Default, Debug)]
pub struct Row {
    pub source_hwnd: isize,
    pub source_pid: u32,
    pub result: &'static str,
    pub detect_to_hide_us: u128,
    pub hide_us: u128,
    pub source_tab_us: u128,
    pub source_registered_us: u128,
    pub new_tab_us: u128,
    pub dest_registered_us: u128,
    pub navigate_us: u128,
    pub source_closed_us: u128,
    pub total_us: u128,
    /// Detect -> "the new tab shows the right folder": what the user waits for.
    pub perceived_us: u128,
    pub visible_at_detect: bool,
    pub source_tabs: usize,
    pub main_hwnd: isize,
}

impl Row {
    pub fn emit(&self) {
        let fields = [
            format!("0x{:x}", self.source_hwnd),
            self.source_pid.to_string(),
            self.result.to_string(),
            self.detect_to_hide_us.to_string(),
            self.hide_us.to_string(),
            self.source_tab_us.to_string(),
            self.source_registered_us.to_string(),
            self.new_tab_us.to_string(),
            self.dest_registered_us.to_string(),
            self.navigate_us.to_string(),
            self.source_closed_us.to_string(),
            self.total_us.to_string(),
            self.perceived_us.to_string(),
            (self.visible_at_detect as u8).to_string(),
            self.source_tabs.to_string(),
            format!("0x{:x}", self.main_hwnd),
        ];
        emit(&fields.join(","));
    }
}
