//! Control channel: a loopback TCP socket so `exptab on|off|toggle|status|quit`
//! can talk to the running daemon without a window, a tray icon or COM.
//!
//! Loopback only: no firewall prompt, no external reachability.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::cfg::Config;

pub const PORT_BASE: u16 = 45999;
const PORT_TRIES: u16 = 8;

pub struct Shared {
    pub enabled: AtomicBool,
    pub quit: AtomicBool,
    pub cfg: Mutex<Config>,
    pub started: Instant,
    pub main_hwnd: AtomicIsize,
    pub merges: AtomicU64,
    pub failures: AtomicU64,
    pub bypassed: AtomicU64,
}

impl Shared {
    pub fn new(cfg: Config) -> Self {
        Self {
            enabled: AtomicBool::new(cfg.enabled),
            quit: AtomicBool::new(false),
            cfg: Mutex::new(cfg),
            started: Instant::now(),
            main_hwnd: AtomicIsize::new(0),
            merges: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            bypassed: AtomicU64::new(0),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
        if let Ok(mut cfg) = self.cfg.lock() {
            cfg.enabled = on;
            let _ = cfg.save();
        }
    }
}

fn port_path() -> std::path::PathBuf {
    crate::cfg::data_dir().join("ipc.port")
}

/// Bind the control socket; `None` means another instance already owns it.
pub fn bind() -> Option<(TcpListener, u16)> {
    for port in PORT_BASE..PORT_BASE + PORT_TRIES {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            let _ = std::fs::write(port_path(), port.to_string());
            return Some((listener, port));
        }
    }
    None
}

pub fn client(command: &str) -> std::io::Result<String> {
    let port: u16 = std::fs::read_to_string(port_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(PORT_BASE);
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.write_all(command.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(reply)
}

pub fn server(listener: TcpListener, shared: Arc<Shared>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let shared = shared.clone();
        std::thread::spawn(move || {
            let _ = handle_conn(stream, shared);
        });
    }
}

fn handle_conn(mut stream: TcpStream, shared: Arc<Shared>) -> std::io::Result<()> {
    let mut line = String::new();
    {
        let mut reader = BufReader::new(stream.try_clone()?);
        reader.read_line(&mut line)?;
    }
    let command = line.trim().to_ascii_lowercase();
    let reply = match command.as_str() {
        "on" | "enable" | "start" => {
            shared.set_enabled(true);
            "enabled\n".to_string()
        }
        "off" | "disable" => {
            shared.set_enabled(false);
            "disabled\n".to_string()
        }
        "toggle" => {
            let next = !shared.is_enabled();
            shared.set_enabled(next);
            format!("{}\n", if next { "enabled" } else { "disabled" })
        }
        "stop" | "quit" | "exit" => {
            shared.quit.store(true, Ordering::Relaxed);
            "stopping\n".to_string()
        }
        "status" | "" => status(&shared),
        "metrics" => last_metrics(8),
        "rescue" | "fix" => format!("rescued {} window(s)\n", crate::win::rescue_hidden()),
        other => format!("unknown command {other:?}\n(use on|off|toggle|status|metrics|rescue|quit)\n"),
    };
    stream.write_all(reply.as_bytes())?;
    stream.flush()?;
    let _ = stream.shutdown(std::net::Shutdown::Both);
    Ok(())
}

fn last_metrics(rows: usize) -> String {
    match std::fs::read_to_string(crate::cfg::metrics_path()) {
        Ok(text) => {
            let mut lines: Vec<&str> = text.lines().collect();
            let header = lines.first().copied().unwrap_or("");
            if lines.len() > rows + 1 {
                lines = lines.split_off(lines.len() - rows);
            }
            format!("{header}\n{}\n", lines.join("\n"))
        }
        Err(e) => format!("cannot read metrics: {e}\n"),
    }
}

fn status(shared: &Shared) -> String {
    let up = shared.started.elapsed().as_secs();
    let main = shared.main_hwnd.load(Ordering::Relaxed);
    let cfg = shared.cfg.lock().map(|c| c.clone()).unwrap_or_default();
    format!(
        "exptab daemon\n\
         state      : {}\n\
         uptime     : {up}s\n\
         main window: 0x{main:x}\n\
         merges     : ok={} failed={} bypassed={}\n\
         poll       : {}ms (idle {}ms)\n\
         max wait   : {}ms\n\
         metrics    : {}\n\
         config     : {}\n",
        if shared.is_enabled() {
            "ENABLED"
        } else {
            "DISABLED"
        },
        shared.merges.load(Ordering::Relaxed),
        shared.failures.load(Ordering::Relaxed),
        shared.bypassed.load(Ordering::Relaxed),
        cfg.detect_poll_ms,
        cfg.detect_poll_idle_ms,
        cfg.max_wait_ms,
        if cfg.metrics { "on" } else { "off" },
        crate::cfg::config_path().display(),
    )
}

/// Re-launch ourselves as `__daemon` with no console of its own.
pub fn spawn_detached() -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .arg("__daemon")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}
