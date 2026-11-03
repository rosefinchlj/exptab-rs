//! The daemon engine.
//!
//! Two kinds of work:
//!   * a detection thread polls `FindWindowExW(NULL, ..., "CabinetWClass")`
//!     every ~2 ms (~14 us per scan) and, the moment a new Explorer window
//!     exists, hands it to a short-lived worker thread that hides it;
//!   * a single merger thread owns COM/IShellWindows and the main window, and
//!     processes queued windows one at a time (merge = new tab + Navigate2 +
//!     close the source window).
//!
//! Serialising merges on one thread keeps 0xA21B/tab discovery race-free and
//! lets one IShellWindows proxy be reused for every snapshot.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::UI::Shell::IShellWindows;
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::cfg::Config;
use crate::com;
use crate::ipc::Shared;
use crate::metrics::Row;
use crate::win;

/// Window handles cross threads as `isize` (HWND is not `Send`).
pub struct Job {
    src: isize,
    src_tab: isize,
    pid: u32,
    t0: Instant,
    hide_us: u128,
    detect_to_hide_us: u128,
    tab_us: u128,
    tabs: usize,
    visible_at_detect: bool,
}

/// Windows handed to the merger and not finished yet. The visibility watchdog
/// must stop as soon as a window is released, otherwise it would keep hiding a
/// window the merger already decided to give back to the user.
static PENDING: Mutex<Option<HashSet<isize>>> = Mutex::new(None);

fn pending_add(id: isize) {
    let mut guard = PENDING.lock().unwrap();
    guard.get_or_insert_with(HashSet::new).insert(id);
}

fn pending_remove(id: isize) {
    if let Some(set) = PENDING.lock().unwrap().as_mut() {
        set.remove(&id);
    }
}

fn pending_contains(id: isize) -> bool {
    PENDING
        .lock()
        .unwrap()
        .get_or_insert_with(HashSet::new)
        .contains(&id)
}

pub fn run(shared: Arc<Shared>) {
    let cfg = shared.cfg.lock().map(|c| c.clone()).unwrap_or_default();

    // Windows left invisible by a previous crash of this daemon.
    recover_hidden_windows();

    let shared_merger = shared.clone();
    let (tx, rx) = mpsc::channel::<Job>();
    thread::spawn(move || merger_loop(rx, shared_merger));

    let mut known: HashSet<isize> = win::cabinets().iter().map(|h| win::id_of(*h)).collect();
    // A pinned main window (config `main_hwnd`) wins over auto-detection.
    if cfg.main_hwnd != 0 && win::is_window(win::to_hwnd(cfg.main_hwnd)) {
        shared.main_hwnd.store(cfg.main_hwnd, Ordering::Relaxed);
    }
    if shared.main_hwnd.load(Ordering::Relaxed) == 0 {
        if let Some(main) = pick_main(&known) {
            shared.main_hwnd.store(win::id_of(main), Ordering::Relaxed);
        }
    }

    let mut seen = win::cabinets();
    let mut last_sweep = Instant::now();
    let mut wait_until_us = win::now_us() as u64;
    let mut idle = false;
    let mut last_foreground = win::id_of(win::foreground_window());
    while !shared.quit.load(Ordering::Relaxed) {
        let sweep_started_us = win::now_us() as u64;
        // Idle-aware detection: while every known window is still in place this
        // is a single `FindWindowExW` that returns immediately (typically
        // "no window"/"no new window"), and the sleep below is an event-driven
        // wait. Full enumeration only happens once a second or when a window
        // went away.
        let mut fresh: Vec<windows::Win32::Foundation::HWND> = Vec::new();
        while let Some(hwnd) = win::new_cabinet(&seen) {
            seen.push(hwnd);
            fresh.push(hwnd);
        }
        for hwnd in fresh {
            let id = win::id_of(hwnd);
            if !known.insert(id) {
                continue;
            }
            // The new (WinUI3/XAML) Explorer window from Windows 11 26H2 is not
            // a classic tab window: no ShellTabWindowClass child, no magic tab
            // messages, and touching its visibility leaves it blank/unclosable.
            // Leave such windows completely alone.
            if win::is_xaml_window(hwnd) {
                continue;
            }
            if !shared.is_enabled() {
                continue;
            }
            let cfg = cfg.clone();
            let shared = shared.clone();
            let tx = tx.clone();
            thread::spawn(move || worker(id, cfg, shared, tx));
        }
        if last_sweep.elapsed() > Duration::from_secs(1) {
            last_sweep = Instant::now();            // Cheap "may anything be merged at all?" test. With no Explorer
            // window on screen there is no destination, so detection may idle
            // at `detect_poll_idle_ms`; while a window is open the 2 ms poll is
            // kept so the new window is still hidden before it appears.
            idle = !win::has_merge_destination();
            // Drop windows that closed since the last full enumeration. The
            // incremental scan only looks at windows *after* `seen`, so a
            // vanished handle would otherwise hide every window created after
            // it.
            if seen.iter().any(|h| !win::is_window(*h)) {
                seen = win::cabinets();
            }            known.retain(|id| win::is_window(win::to_hwnd(*id)));
            let main = shared.main_hwnd.load(Ordering::Relaxed);
            if main != 0 && !win::is_window(win::to_hwnd(main)) {
                shared.main_hwnd.store(0, Ordering::Relaxed);
            }
            // Self-heal: a window this tool parked and then released (failed
            // merge, crash of a previous daemon) must never stay invisible.
            // Explorer's pooled spare window - parked off-screen but still
            // `IsWindowVisible` - is not ours to move, and new-style WinUI3
            // windows break when touched; both are skipped.
            let cabinets = win::cabinets();
            for hwnd in &cabinets {
                let id = win::id_of(*hwnd);
                if pending_contains(id) || win::is_new_style_explorer(*hwnd) {
                    continue;
                }
                if win::parked_by_us(*hwnd) {
                    win::unhide(*hwnd);
                }
            }
            // Second half of the same self-heal, for the other failure shape: a
            // window that sits at a perfectly normal on-screen rect yet fails
            // `IsWindowVisible()` - the user sees it in the taskbar and nothing
            // on the desktop, with no way to bring it back. Explorer's pooled
            // spare window is excluded because it is deliberately parked
            // off-screen; every other classic cabinet showing this signature is
            // ours to repair. Only the main window is touched: a *source* window
            // that is mid-merge legitimately runs with this signature for a few
            // hundred milliseconds, and it is `pending_contains` anyway.
            let main_id = shared.main_hwnd.load(Ordering::Relaxed);
            if main_id != 0 && !pending_contains(main_id) {
                let main_hwnd = win::to_hwnd(main_id);
                if win::is_window(main_hwnd)
                    && !win::is_new_style_explorer(main_hwnd)
                    && !win::is_offscreen(main_hwnd)
                    && !win::is_visible(main_hwnd)
                    && !win::hidden_by_us(main_hwnd)
                {
                    win::show_async(main_hwnd, windows::Win32::UI::WindowsAndMessaging::SW_SHOWNA);
                }
            }
            // Cheap wake for "the Explorer window the user was looking at got
            // hidden/closed while the mouse sits still": the real transition is
            // foreground window X -> some other top-level window, i.e. the
            // *previous* window changed, which needs no hook on upcoming events.
            let foreground = win::id_of(win::foreground_window());
            if foreground != last_foreground {
                last_foreground = foreground;
                win::poke_detection();
            }
        }
        wait_until_us = sweep_started_us;
        // Block until Explorer creates/shows a window, the foreground window
        // changes, or the tick elapses. While nothing happens the thread is
        // asleep in the kernel, so idle CPU stays near zero: no polling tick is
        // needed, because both transitions we care about (a window appearing,
        // the user switching windows) are delivered as events by the hook.
        // The tick is only a safety net for a missed event.
        let tick_ms = if idle { cfg.detect_poll_idle_ms } else { cfg.detect_poll_ms };
        win::wait_detection_event(tick_ms.max(1) as u32);
    }
}

fn worker(src_id: isize, cfg: Config, shared: Arc<Shared>, tx: mpsc::Sender<Job>) {
    let src = win::to_hwnd(src_id);
    let t0 = Instant::now();
    let pid = win::pid_of(src);
    let visible_at_detect = win::is_visible(src);
    // Claim the window for the whole worker lifetime: the periodic self-heal
    // sweep must not "rescue" it while we are hiding/merging it.
    pending_add(src_id);

    // Getting rid of the window must not disturb Explorer while it is still
    // building the shell browser: a synchronous SetWindowLongPtrW/SetWindowPos
    // on a not-yet-pumping thread delays its IShellWindows registration by
    // ~400 ms. So the worker only issues the async, non-blocking SW_HIDE here
    // (18 us) and the merger applies the layered/off-screen hide once the
    // window is registered (merge_one, ~250 ms later).
    // If the daemon has just started and there is no main window yet, this
    // window is probably about to *become* the main window. Hiding it and
    // un-hiding it a second later is exactly the "flash once, then it appears"
    // the user sees on the first open, so leave it alone in that case.
    let no_main = shared.main_hwnd.load(Ordering::Relaxed) == 0;
    let adopt_candidate = no_main && other_tabbed_window(src_id).is_none();
    let mut hidden = false;
    let t_hide = Instant::now();
    if !adopt_candidate && std::env::var("EXPTAB_NOHIDE").is_err() {
        // Park it off-screen right away with an async move (no blocking on the
        // still-initialising Explorer thread), so Explorer's own later
        // ShowWindow cannot make it visible.
        win::hide_async(src);
        hidden = true;
    }
    let hide_us = t_hide.elapsed().as_micros();
    let detect_to_hide_us = t0.elapsed().as_micros();

    let tabs = wait_tabs(src, Duration::from_millis(1500));
    let tab_us = t0.elapsed().as_micros();
    if tabs.is_empty() || !win::is_window(src) {
        pending_remove(src_id);
        win::unhide(src);
        shared.bypassed.fetch_add(1, Ordering::Relaxed);
        return;
    }
    // Not a browser window at all: Explorer shows an access-denied/not-found
    // dialog (#32770) instead of navigating. Such a window never registers in
    // IShellWindows, so release it immediately instead of hiding it for 3 s.
    if win::has_owned_dialog(src) {
        pending_remove(src_id);
        win::unhide(src);
        shared.bypassed.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if cfg.merge_single_tab_only && tabs.len() > 1 {
        pending_remove(src_id);
        win::unhide(src);
        shared.bypassed.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if cfg.bypass_ctrl_shift && win::ctrl_shift_down() {
        pending_remove(src_id);
        win::unhide(src);
        shared.bypassed.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if !shared.is_enabled() {
        pending_remove(src_id);
        win::unhide(src);
        return;
    }

    // Just-started daemon with nothing to merge into: the first window becomes
    // the main window and is released immediately, instead of being hidden and
    // then abandoned (the old behaviour left the user with a lost window).
    if shared.main_hwnd.load(Ordering::Relaxed) == 0 && other_tabbed_window(src_id).is_none() {
        shared.main_hwnd.store(src_id, Ordering::Relaxed);
        pending_remove(src_id);
        if hidden {
            win::unhide(src);
        }
        shared.bypassed.fetch_add(1, Ordering::Relaxed);
        return;
    }


    if !hidden {
        // We skipped the initial hide because this window looked like the
        // main-window candidate, but another window took that role: hide it
        // now, before the merge starts.
        win::hide_async(src);
    }

    let job = Job {
        src: src_id,
        src_tab: win::id_of(tabs[0]),
        pid,
        t0,
        hide_us,
        detect_to_hide_us,
        tab_us,
        tabs: tabs.len(),
        visible_at_detect,
    };
    if tx.send(job).is_err() {
        pending_remove(src_id);
        win::unhide(src);
        return;
    }
    // Explorer re-shows the window with its own ShowWindow() during startup,
    // which would override our async SW_HIDE. Keep a 1 ms watchdog: the moment
    // the window becomes visible we apply the real (layered + off-screen) hide.
    // By then Explorer has finished initialising, so the synchronous call is
    // cheap and cannot delay IShellWindows registration.
    thread::spawn(move || watch_visibility(src_id));
}

fn watch_visibility(src_id: isize) {
    let src = win::to_hwnd(src_id);
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(3000) {
        // The merger may have released this window (failed merge, no main
        // window, URL empty): stop immediately so we do not hide it again.
        if !pending_contains(src_id) {
            return;
        }
        if !win::is_window(src) {
            return;
        }
        if win::hidden_by_us(src) {
            return;
        }
        if win::is_visible(src) {
            // Re-park it off-screen asynchronously (~20 us): if Explorer just
            // called ShowWindow during its initialisation, this gets it out of
            // sight within a millisecond instead of waiting for the blocking
            // layered hide (~90-220 ms).
            win::hide_async(src);
            win::hide(src);
            return;
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn wait_tabs(window: windows::Win32::Foundation::HWND, timeout: Duration) -> Vec<windows::Win32::Foundation::HWND> {
    let start = Instant::now();
    loop {
        let tabs = win::children_of_class(window, win::TAB);
        if !tabs.is_empty() || start.elapsed() > timeout {
            return tabs;
        }
        // A new-style (WinUI3) Explorer window never gets a classic tab host
        // and must not be hidden/merged: hiding one leaves its XAML content
        // blank and unclosable. The combination "XAML chrome but no tab host"
        // can only match such a window - a classic one gets its tab host
        // ~15-20 ms after creation, while its XAML title bar shows up at
        // ~450 ms, long after.
        if start.elapsed() > Duration::from_millis(60) && win::is_new_style_explorer(window) {
            return Vec::new();
        }
        thread::sleep(Duration::from_millis(2));
    }
}

fn pick_main(known: &HashSet<isize>) -> Option<windows::Win32::Foundation::HWND> {
    win::cabinets().into_iter().find(|h| {
        known.contains(&win::id_of(*h))
            && !win::hidden_by_us(*h)
            && !win::is_offscreen(*h)
            && !win::children_of_class(*h, win::TAB).is_empty()
    })
}

/// A window (other than `src`) that already has tabs and is visible to the
/// user: the only kind of window a new one may be merged into.
fn other_tabbed_window(src: isize) -> Option<windows::Win32::Foundation::HWND> {
    win::cabinets().into_iter().find(|h| {
        win::id_of(*h) != src
            && !win::hidden_by_us(*h)
            && !win::is_offscreen(*h)
            && !win::children_of_class(*h, win::TAB).is_empty()
    })
}

fn merger_loop(rx: mpsc::Receiver<Job>, shared: Arc<Shared>) {
    if let Err(e) = com::init() {
        eprintln!("exptab: COM init failed: {e:?}");
        return;
    }
    let sw = match com::shell_windows() {
        Ok(sw) => sw,
        Err(e) => {
            eprintln!("exptab: cannot create ShellWindows: {e:?}");
            return;
        }
    };
    for job in rx {
        let mut row = Row {
            source_hwnd: job.src,
            source_pid: job.pid,
            visible_at_detect: job.visible_at_detect,
            source_tabs: job.tabs,
            detect_to_hide_us: job.detect_to_hide_us,
            hide_us: job.hide_us,
            source_tab_us: job.tab_us,
            ..Row::default()
        };
        let result = merge_one(&sw, &job, &shared, &mut row);
        pending_remove(job.src);
        row.result = result;
        row.total_us = job.t0.elapsed().as_micros();
        row.emit();
        match result {
            "merged" | "deduped" => {
                shared.merges.fetch_add(1, Ordering::Relaxed);
            }
            "no_main_window" => {
                // Nothing to merge into after all: promote this window to main
                // and release it (never leave the user without their window).
                shared.main_hwnd.store(job.src, Ordering::Relaxed);
                win::unhide(win::to_hwnd(job.src));
                shared.bypassed.fetch_add(1, Ordering::Relaxed);
            }
            // Windows that legitimately cannot be merged (Explorer error page,
            // no location) are released, not counted as failures.
            "source_dialog" | "source_url_empty" => {
                win::unhide(win::to_hwnd(job.src));
                shared.bypassed.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                shared.failures.fetch_add(1, Ordering::Relaxed);
                win::unhide(win::to_hwnd(job.src));
            }
        }
    }
}

/// Close a hidden source window: ask its tab to close the window (0xA021 with
/// lParam 0), then fall back to the tab's IWebBrowser2::Quit() and finally
/// WM_CLOSE, so the user never gets a stray window back.
fn close_source(src: windows::Win32::Foundation::HWND, src_tab: windows::Win32::Foundation::HWND, src_ie: &com::Browser) {
    win::magic_tab_command(src_tab, 0);
    if !wait_gone(src, Duration::from_millis(400)) {
        let _ = com::quit(src_ie);
        if !wait_gone(src, Duration::from_millis(1500)) {
            win::close(src);
            let _ = wait_gone(src, Duration::from_millis(800));
        }
    }
}

/// Repeating the same folder open (Win+E, /n,<folder>, Home, This PC, ...)
/// creates a window that is already open as a tab. The hidden source window's
/// title is "<name> - File Explorer"; if the main window already has a tab
/// whose title is `<name>`, this is likely the same view. Return that tab so
/// the caller can verify its URL and skip creating (and immediately closing) a
/// duplicate tab. A false positive is harmless: the merge falls back to the
/// normal path after the URL check.
fn duplicate_hint(
    sw: &IShellWindows,
    main: windows::Win32::Foundation::HWND,
    source_title: &str,
) -> Option<(isize, String)> {
    if source_title.is_empty() {
        return None;
    }
    let main_tabs = tab_ids(main);
    if main_tabs.is_empty() {
        return None;
    }
    for e in com::tab_entries(sw) {
        if e.url.is_empty() || !main_tabs.contains(&e.tab_hwnd) {
            continue;
        }
        let tab_title = win::title_timeout(win::to_hwnd(e.tab_hwnd), 60);
        if tab_title.is_empty() {
            continue;
        }
        let same = source_title == tab_title
            || (source_title.len() > tab_title.len()
                && source_title.starts_with(&tab_title)
                && source_title[tab_title.len()..].starts_with(" - "));
        if same {
            return Some((e.tab_hwnd, e.url));
        }
    }
    None
}

/// Wait until a specific tab shows up in IShellWindows; the (expensive) tab
/// snapshot is only taken when the registered window count changes.
fn wait_source_entry(
    sw: &IShellWindows,
    src_tab: isize,
    deadline: Instant,
) -> Option<(String, com::Browser)> {
    let mut last_count = -1i32;
    loop {
        let count = unsafe { sw.Count().unwrap_or(0) };
        if count != last_count {
            last_count = count;
            if let Some(e) = com::tab_entries(sw)
                .into_iter()
                .find(|e| e.tab_hwnd == src_tab)
            {
                return Some((e.url, e.ie));
            }
        }
        if Instant::now() > deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(3));
    }
}

/// A freshly created source window starts with the generic application title
/// (e.g. "File Explorer") and only gets "<folder> - File Explorer" once its
/// tab has initialised, ~80-100 ms after creation. Poll briefly for the
/// informative title; an empty result means "not a namespace candidate".
fn wait_source_title(src: windows::Win32::Foundation::HWND, timeout: Duration) -> String {
    let start = Instant::now();
    loop {
        let title = win::title_timeout(src, 80);
        if title.contains(" - ") {
            return title;
        }
        if start.elapsed() >= timeout || !win::is_window(src) {
            return String::new();
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn merge_one(sw: &IShellWindows, job: &Job, shared: &Shared, row: &mut Row) -> &'static str {
    let max_wait = shared.cfg.lock().map(|c| c.max_wait_ms).unwrap_or(3000);
    let src = win::to_hwnd(job.src);
    let src_tab = win::to_hwnd(job.src_tab);

    // Create the destination tab first: it takes Explorer ~0.5s to register a
    // new tab in IShellWindows, so starting it before we know the source URL
    // hides that wait behind the source registration instead of adding to it.
    let Some(main) = ensure_main(shared, job.src) else {
        return "no_main_window";
    };
    row.main_hwnd = win::id_of(main);
    let Some(anchor) = win::children_of_class(main, win::TAB).into_iter().next() else {
        return "main_window_has_no_tab";
    };
    // Repeating Win+E re-opens the same namespace window (Home / This PC / ...).
    // The destination tab used to be created before the source had registered,
    // so that duplicate tab was visible for ~0.8 s and then vanished. If the
    // hidden source window's title matches a namespace tab already open in the
    // main window, wait for the source to register first and only create a tab
    // when the URL really differs.
    let mut src_entry: Option<(String, com::Browser)> = None;
    if job.tabs == 1 {
        let source_title = wait_source_title(src, Duration::from_millis(400));
        if let Some((tab, tab_url)) = duplicate_hint(sw, main, &source_title) {
            src_entry = wait_source_entry(
                sw,
                win::id_of(src_tab),
                Instant::now() + Duration::from_millis(max_wait),
            );
            row.source_registered_us = job.t0.elapsed().as_micros();
            if let Some((url, src_ie)) = src_entry.as_ref() {
                if !url.is_empty() && url.eq_ignore_ascii_case(&tab_url) {
                    // Already open: select the existing tab, raise the main
                    // window and close the hidden source. No tab was created.
                    select_tab_handle(main, tab);
                    win::activate_window(main);
                    row.perceived_us = job.t0.elapsed().as_micros();
                    close_source(src, src_tab, src_ie);
                    row.source_closed_us = row.perceived_us;
                    return if win::is_window(src) {
                        "deduped_source_still_open"
                    } else {
                        "deduped"
                    };
                }
            }
        }
    }

    let before = tab_ids(main);

    win::magic_new_tab(anchor);
    let Some(new_tab) = wait_new_tab(main, &before, Duration::from_millis(1200)) else {
        return "new_tab_timeout";
    };
    row.new_tab_us = job.t0.elapsed().as_micros();

    // Now wait for the destination tab's browser (and, on the ordinary path,
    // for the source's URL too). Every registration bump produces a single
    // snapshot that can satisfy both, and the layered hide (which blocks
    // ~100ms) runs on a side thread as soon as the source is known.
    let mut dest: Option<com::Browser> = None;
    let mut last_count = -1i32;
    let mut hide_started = false;
    let mut last_entries: Vec<com::TabEntry> = Vec::new();
    if src_entry.is_some() {
        hide_started = true;
        let src_id = job.src;
        thread::spawn(move || win::hide(win::to_hwnd(src_id)));
    }
    let deadline = Instant::now() + Duration::from_millis(max_wait);
    loop {
        // Access-denied / not-found dialogs mean this window will never
        // register a browser: give the tab we just created back and release it.
        if src_entry.is_none() && win::has_owned_dialog(src) {
            close_own_tab(main, new_tab);
            return "source_dialog";
        }
        let count = unsafe { sw.Count().unwrap_or(0) };
        if count != last_count {
            last_count = count;
            let entries = com::tab_entries(sw);
            last_entries = entries.clone();
            if src_entry.is_none() {
                if let Some(e) = entries.iter().find(|e| e.tab_hwnd == win::id_of(src_tab)) {
                    src_entry = Some((e.url.clone(), e.ie.clone()));
                }
            }
            if dest.is_none() {
                if let Some(e) = entries.iter().find(|e| e.tab_hwnd == win::id_of(new_tab)) {
                    dest = Some(e.ie.clone());
                }
            }
        }
        if !hide_started && src_entry.is_some() {
            hide_started = true;
            row.source_registered_us = job.t0.elapsed().as_micros();
            let src_id = job.src;
            thread::spawn(move || win::hide(win::to_hwnd(src_id)));
        }
        if (src_entry.is_some() && dest.is_some()) || Instant::now() > deadline {
            break;
        }
        thread::sleep(Duration::from_millis(3));
    }
    if !hide_started {
        row.source_registered_us = job.t0.elapsed().as_micros();
        let src_id = job.src;
        thread::spawn(move || win::hide(win::to_hwnd(src_id)));
    }

    let Some((url, src_ie)) = src_entry else {
        close_own_tab(main, new_tab); // drop the empty tab we created
        return "source_not_registered";
    };
    if url.is_empty() {
        close_own_tab(main, new_tab);
        return "source_url_empty";
    }
    let Some(dest) = dest else {
        row.dest_registered_us = job.t0.elapsed().as_micros();
        close_own_tab(main, new_tab);
        return "destination_not_registered";
    };
    row.dest_registered_us = job.t0.elapsed().as_micros();

    // Already open in the main window? Do not add a duplicate tab: drop the
    // empty tab we just created, close the source window, and leave the user's
    // existing tab untouched (this is where we can be nicer than the C# tool).
    let main_tabs = tab_ids(main);
    let duplicate = last_entries
        .iter()
        .find(|e| {
            !e.url.is_empty()
                && e.url.eq_ignore_ascii_case(&url)
                && main_tabs.contains(&e.tab_hwnd)
                && e.tab_hwnd != win::id_of(new_tab)
        })
        .map(|e| e.tab_hwnd);
    if let Some(tab) = duplicate {
        close_own_tab(main, new_tab);
        // Select the existing tab that already shows this folder, so pressing
        // Win+E lands the user on the Home/folder tab instead of an arbitrary
        // one.
        select_tab_handle(main, tab);
        // The folder is already open in the main window: raise it immediately so
        // the user sees the result of Win+E; the source window is invisible.
        win::activate_window(main);

        close_source(src, src_tab, &src_ie);
        row.perceived_us = job.t0.elapsed().as_micros();
        row.source_closed_us = row.perceived_us;
        return if win::is_window(src) { "deduped_source_still_open" } else { "deduped" };
    }

    let t_nav = Instant::now();
    if let Err(e) = com::navigate(&dest, &url) {
        eprintln!("exptab: Navigate2 failed: {e:?}");
        close_own_tab(main, new_tab); // drop the empty tab we created
        return "navigate_failed";
    }
    row.navigate_us = t_nav.elapsed().as_micros();
    // Perceived latency: the tab now shows the right folder.
    row.perceived_us = job.t0.elapsed().as_micros();
    // Raise the main window as soon as the new tab is there: the user asked for
    // a folder, so they must not wait for the (invisible) source window to be
    // closed first.
    win::activate_window(main);

    // Close the hidden source window. 0xA021 with lParam 0 closes the window;
    // if Explorer ignores it, fall back to the tab's IWebBrowser2::Quit() and
    // finally WM_CLOSE, so the user never gets a stray window back.
    let t_close = Instant::now();
    close_source(src, src_tab, &src_ie);
    row.source_closed_us = t_close.elapsed().as_micros();
    if win::is_window(src) {
        "merged_source_still_open"
    } else {
        "merged"
    }
}

fn wait_gone(window: windows::Win32::Foundation::HWND, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if !win::is_window(window) {
            return true;
        }
        thread::sleep(Duration::from_millis(2));
    }
    !win::is_window(window)
}

fn tab_ids(window: windows::Win32::Foundation::HWND) -> Vec<isize> {
    win::children_of_class(window, win::TAB)
        .iter()
        .map(|h| win::id_of(*h))
        .collect()
}

/// Select a tab by its ShellTabWindowClass handle. Explorer's magic 0xA221
/// takes a 1-based *visual* index, while the child enumeration order is the
/// Z-order (the active tab is first), i.e. roughly the reverse of the visual
/// order. Try the computed index first; if the active tab does not switch to
/// the wanted one, probe every index (the mapping is not guaranteed).
fn select_tab_handle(main: windows::Win32::Foundation::HWND, tab: isize) -> bool {
    let ids = tab_ids(main);
    if ids.is_empty() {
        return false;
    }
    if ids[0] == tab {
        return true;
    }
    if let Some(pos) = ids.iter().position(|t| *t == tab) {
        if select_and_confirm(main, tab, (ids.len() - 1 - pos) as i32 + 1) {
            return true;
        }
    }
    for i in 1..=ids.len() as i32 {
        if select_and_confirm(main, tab, i) {
            return true;
        }
    }
    false
}

fn select_and_confirm(main: windows::Win32::Foundation::HWND, tab: isize, one_based: i32) -> bool {
    win::magic_select_tab(main, one_based);
    for _ in 0..8 {
        thread::sleep(Duration::from_millis(15));
        if tab_ids(main).first().copied() == Some(tab) {
            return true;
        }
    }
    false
}

/// Close one tab we created ourselves (Ctrl+W semantics, 0xA021 lParam 1).
///
/// Right after a tab is created Explorer sometimes ignores the command (the
/// tab is still initialising), which used to leave a stray duplicate tab
/// behind, so the command is retried a few times and verified.
fn close_own_tab(main: windows::Win32::Foundation::HWND, tab: windows::Win32::Foundation::HWND) {
    let id = win::id_of(tab);
    for _ in 0..4 {
        // Never risk the last tab: closing it closes the whole window.
        if tab_ids(main).len() <= 1 {
            return;
        }
        win::magic_tab_command(tab, 1);
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(350) {
            if !tab_ids(main).contains(&id) || !win::is_window(tab) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn wait_new_tab(
    main: windows::Win32::Foundation::HWND,
    before: &[isize],
    timeout: Duration,
) -> Option<windows::Win32::Foundation::HWND> {
    let start = Instant::now();
    loop {
        if let Some(h) = win::children_of_class(main, win::TAB)
            .into_iter()
            .find(|h| !before.contains(&win::id_of(*h)))
        {
            return Some(h);
        }
        if start.elapsed() > timeout {
            return None;
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn ensure_main(shared: &Shared, src: isize) -> Option<windows::Win32::Foundation::HWND> {
    let current = shared.main_hwnd.load(Ordering::Relaxed);
    if current != 0 {
        let hwnd = win::to_hwnd(current);
        if win::is_window(hwnd)
            && current != src
            && !win::hidden_by_us(hwnd)
            && !win::is_offscreen(hwnd)
            && !win::children_of_class(hwnd, win::TAB).is_empty()
        {
            return Some(hwnd);
        }
    }
    let picked = other_tabbed_window(src)?;
    shared.main_hwnd.store(win::id_of(picked), Ordering::Relaxed);
    Some(picked)
}

/// Undo "hidden by a daemon that died" state: layered alpha 0 + parked at
/// (-32000, -32000) is our signature; also restores windows parked off-screen
/// by builds that did not remember the original position.
fn recover_hidden_windows() {
    let n = win::rescue_hidden();
    if n > 0 {
        println!("exptab: restored {n} window(s) left hidden by a previous run");
    }
}
