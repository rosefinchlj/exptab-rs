//! P0 feasibility probe, round 9.
//!
//! Round 8 findings:
//!   * `DwmSetWindowAttribute(DWMWA_CLOAK)` cross-process -> E_ACCESSDENIED: dead end.
//!   * `ShowWindowAsync(SW_HIDE)` costs ~25 us (posted, never blocks) and Explorer
//!     did not re-show within 420 ms.
//!   * top-level EnumWindows + class filter costs 1.6-2 ms per scan -> a 1 ms
//!     poll would burn a whole core. Need a cheaper detection primitive.
//!
//! This round tests:
//!   [A] FindWindowExW(NULL, after, "CabinetWClass") scan cost vs EnumWindows
//!   [B] whether ShowWindowAsync(SW_HIDE) at detect persists (2.5 s watch) and
//!       how long the window takes to register in IShellWindows
//!   [C] full merge chain with the cheap-hide strategy and a light COM poll

use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, SW_HIDE};

use crate::{com, win};

fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// Cheap: let user32 do the class match server-side.
fn cabinets_fast() -> Vec<HWND> {
    let cls = win::wide(win::CABINET);
    let mut out = Vec::new();
    let mut after = HWND(std::ptr::null_mut());
    loop {
        let found = unsafe {
            FindWindowExW(
                HWND(std::ptr::null_mut()),
                after,
                PCWSTR(cls.as_ptr()),
                PCWSTR(std::ptr::null()),
            )
        };
        match found {
            Ok(h) if !h.0.is_null() => {
                out.push(h);
                after = h;
            }
            _ => break,
        }
    }
    out
}

fn tab_hwnds(window: HWND) -> Vec<HWND> {
    win::children_of_class(window, win::TAB)
}

fn tab_ids(window: HWND) -> Vec<isize> {
    tab_hwnds(window).iter().map(|h| h.0 as isize).collect()
}

fn first_tab(window: HWND) -> Option<HWND> {
    tab_hwnds(window).into_iter().next()
}

fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let base = std::env::var("TEMP").unwrap_or_else(|_| r"C:\Users\Public".into());
    let p = std::path::PathBuf::from(base)
        .join("exptab-p0")
        .join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&p);
    p
}

fn spawn_detached(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer.exe")
        .arg(format!("/n,{}", path.display()))
        .spawn();
}

fn wait_new(src_before: &[isize], timeout: Duration) -> Option<(HWND, u128)> {
    let t = Instant::now();
    loop {
        for h in cabinets_fast() {
            if !src_before.contains(&(h.0 as isize)) {
                return Some((h, t.elapsed().as_micros()));
            }
        }
        if t.elapsed() > timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn cleanup(hwnd: HWND) {
    if !win::is_window(hwnd) {
        return;
    }
    let _ = win::set_cloak(hwnd, false);
    win::unhide(hwnd);
    if let Some(tab) = first_tab(hwnd) {
        win::magic_tab_command(tab, 0);
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(1200) && win::is_window(hwnd) {
            sleep_ms(5);
        }
    }
    if win::is_window(hwnd) {
        win::close(hwnd);
        sleep_ms(300);
    }
    if win::is_window(hwnd) {
        let pid = win::pid_of(hwnd);
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
        sleep_ms(400);
    }
}

fn print_state(header: &str) {
    println!("[{header}]");
    for w in cabinets_fast() {
        println!(
            "  window=0x{:x} pid={} tabs={:?} vis={} title={:?}",
            w.0 as isize,
            win::pid_of(w),
            tab_ids(w),
            win::is_visible(w),
            win::title(w)
        );
    }
}

fn stats(mut v: Vec<u128>) -> (u128, u128, u128) {
    v.sort_unstable();
    (v[0], v[v.len() / 2], *v.last().unwrap())
}

pub fn run() -> windows::core::Result<()> {
    println!("== exptab P0 probe (round 9: ShowWindowAsync hide) ==");
    com::init()?;
    let base = Instant::now();
    let us = || base.elapsed().as_micros();
    let sw = com::shell_windows()?;

    let known: Vec<isize> = cabinets_fast().iter().map(|h| h.0 as isize).collect();
    let base0 = known.clone();
    print_state("state");

    // ---------------------------------------------------------------- [A]
    println!("\n[A] detection primitive cost");
    let mut enum_costs = Vec::new();
    let mut find_costs = Vec::new();
    for _ in 0..50 {
        let t = Instant::now();
        let v = win::windows_of_class(win::CABINET);
        enum_costs.push(t.elapsed().as_micros());
        std::hint::black_box(&v);
        let t = Instant::now();
        let v = cabinets_fast();
        find_costs.push(t.elapsed().as_micros());
        std::hint::black_box(&v);
    }
    let (emn, emp, emx) = stats(enum_costs);
    let (fmn, fmp, fmx) = stats(find_costs);
    println!("  EnumWindows+filter   min={emn}us p50={emp}us max={emx}us");
    println!("  FindWindowEx(server) min={fmn}us p50={fmp}us max={fmx}us");
    let mut vis_costs = Vec::new();
    let any = cabinets_fast()[0];
    for _ in 0..50 {
        let t = Instant::now();
        std::hint::black_box(win::is_visible(any));
        vis_costs.push(t.elapsed().as_micros());
    }
    let (vmn, vmp, vmx) = stats(vis_costs);
    println!("  IsWindowVisible      min={vmn}us p50={vmp}us max={vmx}us");

    // ---------------------------------------------------------- target window
    let dir_target = scratch_dir("target");
    let dir_src = scratch_dir("src");
    println!(
        "\n[0] scratch dirs target={} src={}",
        dir_target.display(),
        dir_src.display()
    );
    spawn_detached(&dir_target);
    let mut known = known.clone();
    let Some((target, target_us)) = wait_new(&known, Duration::from_secs(10)) else {
        println!("  no target window; aborting");
        return Ok(());
    };
    known.push(target.0 as isize);
    sleep_ms(1200);
    println!(
        "  target=0x{:x} detected_at={target_us}us tabs={:?} title={:?}",
        target.0 as isize,
        tab_ids(target),
        win::title(target)
    );
    let base_count = unsafe { sw.Count().unwrap_or(0) };
    let base_entries: Vec<isize> = com::tab_entries(&sw).iter().map(|e| e.tab_hwnd).collect();
    println!("  baseline count={base_count} entries={base_entries:?}");

    // ---------------------------------- [B] SW_HIDE persistence + registration
    println!("\n[B] ShowWindowAsync(SW_HIDE) at detect");
    let spawn_at = us();
    spawn_detached(&dir_src);
    let Some((src, detect_us)) = wait_new(&known, Duration::from_secs(10)) else {
        println!("  no source window; aborting");
        cleanup(target);
        return Ok(());
    };
    known.push(src.0 as isize);
    let t_hide = Instant::now();
    let ok = win::show_async(src, SW_HIDE);
    let hide_us = t_hide.elapsed().as_micros();
    println!(
        "  src=0x{:x} detect={detect_us}us visible_at_detect={} hide_call={hide_us}us ok={ok}",
        src.0 as isize,
        win::is_visible(src)
    );

    // Watch visibility (cheap) and registration (Count -> snapshot only on change).
    let mut vis_events: Vec<(u128, bool)> = Vec::new();
    let mut last_vis = win::is_visible(src);
    vis_events.push((us(), last_vis));
    let mut reg: Option<(isize, u128, String)> = None;
    let mut count_calls = 0u32;
    let mut count_cost = 0u128;
    let mut next_poll = Instant::now();
    let watch = Instant::now();
    while watch.elapsed() < Duration::from_secs(2) {
        let v = win::is_visible(src);
        let now = us();
        if v != last_vis {
            vis_events.push((now, v));
            last_vis = v;
        }
        if reg.is_none() && next_poll.elapsed() >= Duration::from_millis(20) {
            next_poll = Instant::now();
            let c0 = Instant::now();
            let n = unsafe { sw.Count().unwrap_or(0) };
            count_cost += c0.elapsed().as_micros();
            count_calls += 1;
            if n != base_count {
                if let Some(e) = com::tab_entries(&sw)
                    .into_iter()
                    .find(|e| !base_entries.contains(&e.tab_hwnd))
                {
                    reg = Some((e.tab_hwnd, now, e.url.clone()));
                    println!(
                        "  REGISTERED at {}us (detect+{}us) tab=0x{:x} url={:?}",
                        now,
                        now - detect_us,
                        e.tab_hwnd,
                        e.url
                    );
                }
            }
        }
        if reg.is_some() && watch.elapsed() > Duration::from_millis(400) {
            break;
        }
        sleep_ms(5);
    }
    println!(
        "  visibility transitions after hide: {:?}",
        vis_events
            .iter()
            .map(|(t, v)| (t - detect_us, *v))
            .collect::<Vec<_>>()
    );
    println!(
        "  Count probes: {count_calls} calls, avg {}us",
        if count_calls > 0 { count_cost / count_calls as u128 } else { 0 }
    );
    println!(
        "  after watch: visible={} cloak={:?} layered={}",
        win::is_visible(src),
        win::get_cloak(src),
        win::is_layered(src)
    );

    // ------------------------------------------------------------- [C] merge
    println!("\n[C] merge with hidden source");
    if let Some((src_tab, reg_us, url)) = reg {
        let Some(target_tab) = first_tab(target) else {
            println!("  target has no tab");
            cleanup(src);
            cleanup(target);
            return Ok(());
        };
        let before_target = tab_ids(target);
        let t_new = Instant::now();
        win::magic_new_tab(target_tab);
        let mut new_tab: Option<(isize, u128)> = None;
        while t_new.elapsed() < Duration::from_secs(3) && new_tab.is_none() {
            if let Some(h) = tab_ids(target)
                .into_iter()
                .find(|h| !before_target.contains(h))
            {
                new_tab = Some((h, t_new.elapsed().as_micros()));
            } else {
                sleep_ms(1);
            }
        }
        println!("  new tab: {new_tab:?} after 0xA21B");

        let base_plus_src: Vec<isize> = base_entries.iter().copied().chain([src_tab]).collect();
        let mut dest: Option<com::TabEntry> = None;
        let mut dest_us = 0u128;
        let t_dest = Instant::now();
        let mut polls = 0u32;
        while t_dest.elapsed() < Duration::from_secs(3) && dest.is_none() {
            polls += 1;
            let n = unsafe { sw.Count().unwrap_or(0) };
            if n as usize > base_plus_src.len() {
                if let Some(e) = com::tab_entries(&sw)
                    .into_iter()
                    .find(|e| !base_plus_src.contains(&e.tab_hwnd))
                {
                    dest_us = t_dest.elapsed().as_micros();
                    dest = Some(e);
                    break;
                }
            }
            sleep_ms(2);
        }
        println!(
            "  dest entry after {dest_us}us ({polls} polls) {:?}",
            dest.as_ref().map(|d| (d.tab_hwnd, d.url.clone()))
        );
        if let Some(d) = dest {
            let leaf = url.rsplit('/').next().unwrap_or("").to_string();
            let t_nav = Instant::now();
            match com::navigate(&d.ie, &url) {
                Ok(()) => {
                    let nav_call_us = t_nav.elapsed().as_micros();
                    let mut done_us = 0u128;
                    let t_poll = Instant::now();
                    while t_poll.elapsed() < Duration::from_secs(3) {
                        let cur = com::location_url(&d.ie).unwrap_or_default();
                        if !leaf.is_empty() && cur.contains(&leaf) {
                            done_us = t_poll.elapsed().as_micros();
                            break;
                        }
                        sleep_ms(5);
                    }
                    println!("  Navigate2 call={nav_call_us}us url_confirmed_after=+{done_us}us");
                }
                Err(e) => println!("  Navigate2 ERR {e:?}"),
            }
        }
        let merged_us = us();
        let t_close = Instant::now();
        win::magic_tab_command(HWND(src_tab as *mut core::ffi::c_void), 0);
        while t_close.elapsed() < Duration::from_secs(2) && win::is_window(src) {
            sleep_ms(2);
        }
        println!(
            "  source closed={} after {}us",
            !win::is_window(src),
            t_close.elapsed().as_micros()
        );
        println!(
            "\n  TIMELINE spawn->detect={}us detect->hidden={}us detect->visible_first={:?} detect->registered={}us",
            detect_us,
            hide_us,
            vis_events.iter().find(|(_, v)| *v).map(|(t, _)| t.saturating_sub(detect_us)),
            reg_us.saturating_sub(detect_us)
        );
        println!(
            "  TOTAL spawn->merged = {}ms",
            merged_us.saturating_sub(spawn_at) / 1000
        );
        println!(
            "  src window title after merge={:?} visible={}",
            win::title(target),
            win::is_visible(src)
        );
    } else {
        println!("  source never registered; entries:");
        for e in com::tab_entries(&sw) {
            println!("    tab=0x{:x} url={:?}", e.tab_hwnd, e.url);
        }
        cleanup(src);
    }

    // ---------------------------------------------------------------- [Z]
    println!("\n[Z] cleanup");
    cleanup(target);
    for w in cabinets_fast() {
        if !base0.contains(&(w.0 as isize)) {
            println!("  leftover 0x{:x} title={:?}", w.0 as isize, win::title(w));
            cleanup(w);
        }
    }
    let sw = com::shell_windows()?;
    for h in &base0 {
        let w = HWND(*h as *mut core::ffi::c_void);
        if !win::is_window(w) {
            continue;
        }
        for t in tab_hwnds(w) {
            let title = win::title(t);
            let url = com::tab_entries(&sw)
                .into_iter()
                .find(|e| e.tab_hwnd == t.0 as isize)
                .map(|e| e.url)
                .unwrap_or_default();
            if title.starts_with("run-2026")
                || title.starts_with("exptab-")
                || url.to_lowercase().contains("exptab-")
            {
                println!("  closing probe tab 0x{:x} {title:?}", t.0 as isize);
                win::magic_tab_command(t, 1);
                sleep_ms(400);
            }
        }
    }
    print_state("final");
    println!("\nPROBE DONE  (total {}ms)", base.elapsed().as_millis());
    Ok(())
}
