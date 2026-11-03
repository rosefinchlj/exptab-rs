//! exptab — fast auto-merge of new Windows 11 Explorer windows into tabs.
//!
//! Usage:
//!   exptab                 run the daemon in this console
//!   exptab start           run it in the background (no console window)
//!   exptab status|on|off|toggle
//!   exptab quit            stop the background daemon
//!   exptab probe           developer-only P0 probe (touches test windows)

mod cfg;
mod com;
mod engine;
mod ipc;
mod metrics;
mod probe;
mod win;

use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // No arguments: make sure a background daemon is running (double-click
    // friendly). Use `exptab run` for a foreground daemon with a console.
    let command = args.first().map(String::as_str).unwrap_or("start");
    match command {
        "probe" => {
            if let Err(e) = probe::run() {
                eprintln!("probe failed: {e:?}");
                std::process::exit(1);
            }
        }
        "__daemon" | "run" => daemon(),
        "start" => match ipc::client("status") {
            Ok(_) => println!("exptab is already running"),
            Err(_) => match ipc::spawn_detached() {
                Ok(()) => {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    match ipc::client("status") {
                        Ok(reply) => print!("exptab started.\n{reply}"),
                        Err(e) => eprintln!("exptab failed to start: {e}"),
                    }
                }
                Err(e) => eprintln!("cannot spawn daemon: {e}"),
            },
        },
        "status" | "on" | "off" | "enable" | "disable" | "toggle" | "quit" | "stop"
        | "metrics" | "rescue" => {
            let wire = match command {
                "stop" => "quit",
                "enable" => "on",
                "disable" => "off",
                other => other,
            };
            match ipc::client(wire) {
                Ok(reply) => print!("{reply}"),
                Err(e) => {
                    eprintln!(
                        "exptab: no running daemon ({e}).\nstart it with `exptab start` \
                         (or run `exptab` in a console)."
                    );
                    std::process::exit(1);
                }
            }
        }
        "autostart" => {
            match args.get(1).map(String::as_str).unwrap_or("status") {
                "on" | "enable" => match autostart_set(true) {
                    Ok(()) => println!("autostart: enabled - exptab starts with Windows"),
                    Err(e) => {
                        eprintln!("exptab: cannot enable autostart: {e}");
                        std::process::exit(1);
                    }
                },
                "off" | "disable" => match autostart_set(false) {
                    Ok(()) => println!("autostart: disabled"),
                    Err(e) => {
                        eprintln!("exptab: cannot disable autostart: {e}");
                        std::process::exit(1);
                    }
                },
                other => {
                    eprintln!("exptab: autostart {other:?} - use `exptab autostart on|off`");
                    std::process::exit(2);
                }
            }
        }
        "help" | "-h" | "--help" => usage(),
        other => {
            eprintln!("exptab: unknown command {other:?}\n");
            usage();
            std::process::exit(2);
        }
    }
}

/// Register/unregister the daemon in HKCU Run (per-user, no admin needed).
fn autostart_set(on: bool) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let exe = std::env::current_exe()?;
    let quoted = format!("\"{}\" start", exe.display());
    let mut cmd = std::process::Command::new("reg.exe");
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    if on {
        cmd.args(["add", KEY, "/v", "exptab", "/t", "REG_SZ", "/d", &quoted, "/f"]);
    } else {
        cmd.args(["delete", KEY, "/v", "exptab", "/f"]);
    }
    let status = cmd.status()?;
    if !status.success() && on {
        return Err(std::io::Error::other("reg.exe add failed"));
    }
    Ok(())
}

fn daemon() {
    let config = cfg::Config::load();
    metrics::init(config.metrics);

    // Real single-instance guard. The IPC port probe in `ipc::bind` is not enough:
    // it falls through to the next free port, so a second daemon would happily run
    // side by side (two daemons merge the same window twice -> duplicate tabs).
    if !claim_single_instance() {
        eprintln!("exptab: another instance is already running");
        std::process::exit(1);
    }

    let Some((listener, port)) = ipc::bind() else {
        eprintln!("exptab: another instance is already running");
        std::process::exit(1);
    };
    install_console_cleanup();

    let shared = Arc::new(ipc::Shared::new(config));
    {
        let shared = shared.clone();
        std::thread::spawn(move || ipc::server(listener, shared));
    }
    println!(
        "exptab daemon: listening on 127.0.0.1:{port}, config {}, metrics {}",
        cfg::config_path().display(),
        cfg::metrics_path().display()
    );
    engine::run(shared.clone());
    println!("exptab daemon stopping");
}

/// Acquire the process-wide named event; keep the handle alive for the whole run.
fn claim_single_instance() -> bool {
    use std::sync::OnceLock;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateEventW;

    static HANDLE: OnceLock<isize> = OnceLock::new();
    let name = w!("Local\\exptab-rs-daemon");
    let h = unsafe { CreateEventW(None, true, false, PCWSTR(name.as_ptr())) };
    let created = match h {
        Ok(h) => {
            let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            if !already {
                let _ = HANDLE.set(h.0 as isize);
            }
            !already
        }
        // Could not create the kernel object: fall back to the port probe instead of refusing to run.
        Err(_) => true,
    };
    created
}

fn usage() {
    println!(
        "exptab — merge new Explorer windows into tabs of the main window\n\n\
         USAGE:\n  \
         exptab             make sure the daemon is running (background)\n  \
         exptab run         run the daemon in the foreground (console)\n  \
         exptab start       start it in the background (no console)\n  \
         exptab status      show state, uptime, counters and config path\n  \
         exptab on|off      enable / disable merging at runtime\n  \
         exptab toggle      flip the master switch\n  \
         exptab metrics     show the last 8 merge metrics (CSV rows)\n  \
         exptab rescue      un-hide any Explorer window left invisible by a crash\n  \
         exptab autostart on|off   start with Windows / remove that entry\n  \
         exptab quit        stop the daemon\n\n\
         Config: %LOCALAPPDATA%\\exptab\\config.ini\n\
         Metrics: %LOCALAPPDATA%\\exptab\\metrics.csv\n"
    );
}

/// If the daemon is killed while a source window is hidden, put it back:
/// Ctrl+C / console close runs this on a separate thread.
fn install_console_cleanup() {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::System::Console::SetConsoleCtrlHandler;

    unsafe extern "system" fn handler(_ctrl_type: u32) -> BOOL {
        win::rescue_hidden();
        // Give user32 a moment to process the ShowWindow calls, then leave.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(300));
            std::process::exit(0);
        });
        BOOL(1)
    }

    let _ = unsafe { SetConsoleCtrlHandler(Some(handler), true) };
}
