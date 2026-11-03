//! Thin wrappers around user32 window APIs.
use std::collections::HashMap;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};

use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWINDOWATTRIBUTE,};

use windows::Win32::UI::WindowsAndMessaging::*;

pub const CABINET: &str = "CabinetWClass";

pub const TAB: &str = "ShellTabWindowClass";
/// HWND is not `Send` (it wraps a raw pointer), so threads communicate window
/// handles as `isize` and reconstruct them on arrival.
pub fn to_hwnd(id: isize) -> HWND {
    HWND(id as *mut core::ffi::c_void)
}
pub fn id_of(hwnd: HWND) -> isize {
    hwnd.0 as isize
}
pub fn class_name(hwnd: HWND) -> String {
    if hwnd.0.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 256];
    let n = unsafe {
        GetClassNameW(hwnd, &mut buf)
    };

    let n = n.clamp(0, buf.len() as i32) as usize;
    String::from_utf16_lossy(&buf[..n])
}
pub fn title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe {
        GetWindowTextW(hwnd, &mut buf)
    };

    let n = n.clamp(0, buf.len() as i32) as usize;
    String::from_utf16_lossy(&buf[..n])
}
/// Read another process' window title without blocking indefinitely: while a
/// new Explorer window is initialising its UI thread can be busy, and a plain
/// `GetWindowTextW` (which sends WM_GETTEXT) would stall the merge.
pub fn title_timeout(hwnd: HWND, timeout_ms: u32) -> String {
    let mut buf = [0u16; 512];
    let mut copied: usize = 0;
    let r = unsafe {
        SendMessageTimeoutW( hwnd, WM_GETTEXT, WPARAM(buf.len()), LPARAM(buf.as_mut_ptr() as isize), SMTO_ABORTIFHUNG, timeout_ms, Some(&mut copied), )
    };

    if r.0 == 0 {
        return String::new();
    }
    let mut n = copied.min(buf.len());
    if let Some(pos) = buf[..n].iter().position(|c| *c == 0) {
        n = pos;
    } String::from_utf16_lossy(&buf[..n])
}
pub fn is_visible(hwnd: HWND) -> bool {
unsafe {
        IsWindowVisible(hwnd).as_bool()
    }
}
pub fn is_window(hwnd: HWND) -> bool {
unsafe {
        IsWindow(hwnd).as_bool()
    }
}
pub fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid))
    };

    pid
}
pub fn parent_of(hwnd: HWND) -> HWND {
unsafe {
        GetParent(hwnd).unwrap_or_default()
    }
}
unsafe extern "system" 
fn enum_proc(hwnd: HWND, lparam: LPARAM) -> windows::Win32::Foundation::BOOL {
    let out = &mut *(lparam.0 as *mut Vec<HWND>);
    out.push(hwnd);
    windows::Win32::Foundation::BOOL(1)
}
/// Top-level windows in Z order (topmost first).
pub fn top_level_windows() -> Vec<HWND> {
    let mut out: Vec<HWND> = Vec::new();
unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut out as *mut _ as isize));
    } out
}
pub fn windows_of_class(class: &str) -> Vec<HWND> {
    top_level_windows() .into_iter() .filter(|h| class_name(*h) == class) .collect()
}
/// Is there a window a new Explorer window could be merged into right now?
///
/// A merge needs a *visible, on-screen* cabinet that already owns at least one
/// classic tab. When the user has no Explorer window open (or only Explorer's
/// pooled spare window parked off-screen) nothing can be merged, so the
/// detection loop may idle at `detect_poll_idle_ms` instead of the 2 ms merge
/// latency. Cheap by design: one `FindWindowExW` for the common "no window at
/// all" case, and only when something exists does it look at each cabinet.
pub fn has_merge_destination() -> bool {
    let cls = wide(CABINET);
    let cls_ptr = PCWSTR(cls.as_ptr());
    let mut after = HWND(std::ptr::null_mut());
    loop {
        let found =
    unsafe {
            FindWindowExW( HWND(std::ptr::null_mut()), after, cls_ptr, PCWSTR(std::ptr::null()), )
        };

        match found {
            Ok(h) if !h.0.is_null() => {
                if is_visible(h) && !is_offscreen(h) && !hidden_by_us(h) && !children_of_class(h, TAB).is_empty() {
                    return true;
                } after = h;
            } _ => return false,
        }
    }
}
/// Fast scan: user32 does the class match server-side (~14us per scan) instead
/// of walking every top-level window and calling GetClassNameW (~1.2ms).
pub fn cabinets() -> Vec<HWND> {
    let cls = wide(CABINET);
    let mut out = Vec::new();
    let mut after = HWND(std::ptr::null_mut());
    loop {
        let found =
    unsafe {
            FindWindowExW( HWND(std::ptr::null_mut()), after, PCWSTR(cls.as_ptr()), PCWSTR(std::ptr::null()), )
        };

        match found {
            Ok(h) if !h.0.is_null() => {
                out.push(h);
                after = h;
            } _ => break,
        }
    } out
}
/// First `CabinetWClass` window that did not exist in `seen`, or `None`.
///
/// This is the idle fast path of the detection loop: usually Explorer has no
/// top-level window at all (or a handful), so `FindWindowExW` answers with a
/// null handle after one (or a few) cheap user32 calls. Enumerating *all*
/// cabinet handles every 2 ms instead - and waking up 500 times a second to do
/// it - costs a measurable slice of a core for nothing.
pub fn new_cabinet(seen: &[HWND]) -> Option<HWND> {
    let cls = wide(CABINET);
    let cls_ptr = PCWSTR(cls.as_ptr());
    let mut after = HWND(std::ptr::null_mut());
    loop {
        let Ok(h) = (
unsafe {
            FindWindowExW( HWND(std::ptr::null_mut()), after, cls_ptr, PCWSTR(std::ptr::null()), )
        } ) else {
            return None;
        };

        if h.0.is_null() {
            return None;
        } if !seen.contains(&h) {
            return Some(h);
        } after = h;
    }
}
/// Wake the detection loop right away (it blocks in `wait_detection_event`).
pub fn poke_detection() {
use windows::Win32::System::Threading::SetEvent;
    if let Some(hook) = EVENT_HOOK.get() {
        if hook.started {
unsafe {
                let _ = SetEvent(hook.event);
            }
        }
    }
}
/// Foreground top-level window (0 when there is none, e.g. on a locked desktop).
pub fn foreground_window() -> HWND {
unsafe {
        GetForegroundWindow()
    }
}
/// Process-wide waitable timer used for every short sleep of this tool.
///
/// `thread::sleep` waits on a timer whose resolution follows the *system*
/// minimum timer resolution (up to 15.6 ms): sleeping "2 ms" actually takes
/// ~31 ms, wakes the CPU from idle ~32 times a second, and the real delay
/// depends on whether some other app raised the resolution - which is why the
/// idle CPU of earlier builds measured anywhere between 0.3% and 2.7% of a core.
/// A per-process waitable timer keeps 0.5 ms granularity regardless, so the
/// detection loop can be both precise (2 ms) and cheap.
pub struct Heartbeat {
    timer: windows::Win32::Foundation::HANDLE,
}

// A kernel handle that lives for the whole process (used via `Heartbeat::get()`).
unsafe impl Send for Heartbeat {}
unsafe impl Sync for Heartbeat {}

static HEARTBEAT: std::sync::OnceLock<Heartbeat> = std::sync::OnceLock::new();

impl Heartbeat {
    pub fn get() -> &'static Heartbeat {
        HEARTBEAT.get_or_init(|| {
            use windows::Win32::System::Threading::CreateWaitableTimerW;
            let timer = unsafe { CreateWaitableTimerW(None, false, PCWSTR::null()) }
                .unwrap_or(windows::Win32::Foundation::HANDLE(std::ptr::null_mut()));
            Heartbeat { timer }
        })
    }

    fn ok(&self) -> bool {
        !self.timer.is_invalid()
    }
}
/// Monotonic microseconds (QueryPerformanceCounter, ~7 ns resolution).
pub fn now_us() -> u128 {
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    use std::sync::OnceLock;
    static FREQ: OnceLock<u64> = OnceLock::new();
    let freq = *FREQ.get_or_init(|| {
        let mut f = 0i64;
        unsafe { let _ = QueryPerformanceFrequency(&mut f); }
        f.max(1) as u64
    });
    let mut count = 0i64;
    unsafe { let _ = QueryPerformanceCounter(&mut count); }
    (count.max(0) as u128) * 1_000_000 / freq as u128
}
/// Block (without burning CPU) until `deadline_us` in the `now_us` domain.
pub fn wait_until_us(deadline_us: u128) {
    use windows::Win32::System::Threading::{SetWaitableTimer, WaitForSingleObject};
    let beat = Heartbeat::get();
    if !beat.ok() {
        let now = now_us();
        if deadline_us > now {
            thread::sleep(Duration::from_micros((deadline_us - now).min(20_000) as u64));
        }
        return;
    }
    while now_us() < deadline_us {
        let remaining_us = (deadline_us - now_us()) as u64;
        let ms = remaining_us.div_ceil(1_000) as u32;
        // 100 ns units, negative = relative
        let due = -(ms as i64) * 10_000;
        unsafe {
            let _ = SetWaitableTimer(beat.timer, &due, 0, None, None, false);
            let _ = WaitForSingleObject(beat.timer, ms + 2);
        }
    }
}
/// Wait until Explorer creates/shows a window, the user switches the foreground
/// window, or `tick_ms` elapses.
///
/// A bare `sleep()` loop makes the kernel wake a thread hundreds of times a
/// second for nothing. Instead the thread blocks on a WinEvent hook (callback is
/// OUTOFCONTEXT, so nothing is injected into Explorer) plus the waitable timer.
/// The hook is only a wake-up *hint*: correctness still comes from the
/// `FindWindowExW` scan that runs after every wake, plus the once-a-second sweep.
///
/// The foreground change is reported by its own hook (`EVENT_SYSTEM_FOREGROUND`)
/// rather than polled on every tick: polling it meant the thread had to wake
/// ~16 times a second while the machine was idle, which is where most of the
/// measured idle CPU went. `tick_ms` therefore only bounds pathological cases.
pub fn wait_detection_event(tick_ms: u32) -> bool {
    use windows::Win32::Foundation::{WAIT_FAILED, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{ResetEvent, WaitForSingleObject};
    let hook = EVENT_HOOK.get_or_init(EventHook::start);
    if !hook.started {
        thread::sleep(Duration::from_millis(tick_ms.max(1) as u64));
        return true;
    }
    unsafe {
        let _ = ResetEvent(hook.event);
        match WaitForSingleObject(hook.event, tick_ms.max(1)) {
            WAIT_FAILED => {
                thread::sleep(Duration::from_millis(tick_ms.max(1) as u64));
                true
            }
            // Timed out: no Explorer window activity at all.
            WAIT_TIMEOUT => false,
            // Window created/shown, or the foreground window changed.
            _ => true,
        }
    }
}
/// Arm the shared heartbeat timer for `ms` (used only to bound the event wait).
struct EventHook {
    started: bool,
    /// Kept alive for the process lifetime (the hook itself is never removed).
    #[allow(dead_code)]
    hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    /// Manual-reset, so every wait in a burst of windows sees the signal and we
    /// clear it ourselves only after the scan - a callback firing between the
    /// scan and the wait can never be lost.
    event: windows::Win32::Foundation::HANDLE,
}

// Both handles are plain Win32 objects that stay valid for the process
// lifetime; they are only used through `EVENT_HOOK`.
unsafe impl Send for EventHook {}
unsafe impl Sync for EventHook {}

static EVENT_HOOK: std::sync::OnceLock<EventHook> = std::sync::OnceLock::new();

impl EventHook {
    fn start() -> Self {
        use windows::Win32::System::Threading::{CreateEventW, SetEvent};
        use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
        use windows::Win32::UI::WindowsAndMessaging::{
            EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT,
            WINEVENT_SKIPOWNPROCESS,
        };

        /// OUTOFCONTEXT: the callback runs on our own thread, nothing is
        /// injected into Explorer. Any window create/show is only a hint -
        /// the scan after every wake is what actually finds the window.
        unsafe extern "system" fn cb(
            _hook: HWINEVENTHOOK,
            _event: u32,
            hwnd: HWND,
            _id_object: i32,
            _id_child: i32,
            _thread: u32,
            _time: u32,
        ) {
            // A foreground change (the window the user was looking at got
            // hidden, closed or replaced) must wake the loop even though it
            // creates no window.
            if hwnd.0.is_null() {
                return;
            }
            if let Some(hook) = EVENT_HOOK.get() {
                if hook.started {
                    let _ = SetEvent(hook.event);
                }
            }
        }

        // Manual reset: the detection loop clears it right before it waits.
        let event = unsafe { CreateEventW(None, true, false, None) }.unwrap_or_default();
        // One hook spanning both ranges: object create/show (32768..32770) and
        // system foreground (3).
        let hook = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_OBJECT_SHOW,
                None,
                Some(cb),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };

        Self {
            started: !hook.is_invalid() && !event.is_invalid(),
            hook,
            event,
        }
    }
}
/// True while the user holds Ctrl+Shift (used as the "do not merge" override).
pub fn ctrl_shift_down() -> bool {
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_SHIFT};

unsafe {
        let ctrl = GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000 != 0;
        let shift = GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000 != 0;
        ctrl && shift
    }
}
/// Direct children of `parent` whose window class equals `class`.
pub fn children_of_class(parent: HWND, class: &str) -> Vec<HWND> {
    let mut out = Vec::new();
    if parent.0.is_null() {
        return out;
    }
    let cls = wide(class);
    let cls_ptr = PCWSTR(cls.as_ptr());
    let mut after = HWND(std::ptr::null_mut());
    loop {
        let found =
    unsafe {
            FindWindowExW(parent, after, cls_ptr, PCWSTR(std::ptr::null()))
        };

        match found {
            Ok(h) if !h.0.is_null() => {
                out.push(h);
                after = h;
            } _ => break,
        }
    } out
}
/// UTF-16, null-terminated buffer kept alive by the caller.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
/// What a window looked like before we hid it, so [`unhide`] can put it back
/// exactly where the user expects (moving it off-screen is part of the hide,
/// forgetting the original spot is how windows get lost).
#[derive(Clone, Copy)]
struct Saved {
    rect: RECT,
    layered: bool,
    alpha: u8,
}

static SAVED: Mutex<Option<HashMap<isize, Saved>>> = Mutex::new(None);

fn with_saved<R>(f: impl FnOnce(&mut HashMap<isize, Saved>) -> R) -> R {
    let mut guard = SAVED.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}
/// Fallback position for a window whose original spot cannot be trusted.
fn default_rect() -> RECT {
    RECT {
        left: 120,
        top: 90,
        right: 1400,
        bottom: 890,
    }
}
/// Return `rect` if it is a usable on-screen position, else [`default_rect`].
///
/// Explorer parks spare windows at -32000 and reuses them for Win+E; a window
/// detected while parked there must not be "restored" to that position, and a
/// zero-sized rect (window not positioned yet) is just as useless.
fn sane_rect(rect: RECT) -> RECT {
    let w = rect.right - rect.left;
    let h = rect.bottom - rect.top;
    if rect.left <= -30_000 || rect.top <= -30_000 || w < 200 || h < 200 {
        default_rect()
    } else {
        rect
    }
}
/// True when the window sits outside the visible desktop (our hide parks it at
/// -32000, and Explorer reuses such windows for Win+E).
pub fn is_offscreen(hwnd: HWND) -> bool {
    let mut rect = RECT::default();
unsafe {
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return false;
        }
    } rect.left <= -30_000 || rect.top <= -30_000
}
/// Window carries the WinUI3/XAML chrome islands.
///
/// Caution: on Windows 11 26H2 (build 26300) *every* classic Explorer window
/// has these children once it is up and running (the title bar is WinUI3 even
/// for classic tab windows), so on a settled window this alone does not tell
/// the two kinds apart. It is only reliable at the instant a window is created:
/// a classic window has no XAML children yet, a new-style one already does.
pub fn is_xaml_window(hwnd: HWND) -> bool {
    !children_of_class(hwnd, "Microsoft.UI.Content.DesktopChildSiteBridge" ).is_empty() || !children_of_class(hwnd, "TITLE_BAR_SCAFFOLDING_WINDOW_CLASS" ).is_empty()
}
/// New "WinUI 3" File Explorer windows (Windows 11 26H2+) are `CabinetWClass`
/// too, but their tab strip is XAML: there is **no** `ShellTabWindowClass`
/// child at all, so none of the classic tab messages apply and the window does
/// not register in `IShellWindows` the same way. Hiding/moving such a window
/// leaves its XAML content blank and unclosable (observed: white pane, two
/// tabs, no response to WM_CLOSE/UIA), so it must never be touched.
///
/// A settled classic window always has at least one `ShellTabWindowClass`
/// child, hence the combination of "XAML chrome" + "no classic tab host" is a
/// reliable classifier outside the creation-instant window.
pub fn is_new_style_explorer(hwnd: HWND) -> bool {
    children_of_class(hwnd, TAB).is_empty() && is_xaml_window(hwnd)
}
/// True when this window looks parked by *us*: either layered/alpha-0 at
/// -32000 (the synchronous hide), or parked off-screen and invisible
/// (the async hide). Explorer's own spare window is also parked at -32000 but
/// stays `IsWindowVisible() == true`, so it is deliberately not matched —
/// moving that one would corrupt Explorer's Win+E pool.
pub fn parked_by_us(hwnd: HWND) -> bool {
    hidden_by_us(hwnd) || (is_offscreen(hwnd) && !is_visible(hwnd))
}
/// Park the window off-screen without blocking the caller.
/// pump messages yet), so a *synchronous* `SetWindowPos`/`SetWindowLongPtrW`
/// from this thread blocks for 90-220 ms - and that block is what delays
/// `IShellWindows` registration. `SWP_ASYNCWINDOWPOS` posts the move instead of
/// waiting for the window thread, and `ShowWindowAsync(SW_HIDE)` is posted too,
/// so this returns in ~20 us and the window is parked off-screen long before
/// Explorer's own `ShowWindow` call — which is what removes the flash.
pub fn hide_async(hwnd: HWND) {
    if !is_window(hwnd) {
        return;
    }
    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut rect);
    }
    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 };
    let layered = (ex & WS_EX_LAYERED.0) != 0;
    let alpha = if layered { alpha(hwnd).unwrap_or(255) } else { 255 };
    let id = id_of(hwnd);
    // Explorer keeps a spare `主文件夹` window parked at -32000 and shows it when
    // Win+E is pressed; recording *that* as the "original" position would
    // restore the window off-screen. Only sane rects are remembered.
    with_saved(|map| {
        map.entry(id).or_insert(Saved {
            rect: sane_rect(rect),
            layered,
            alpha,
        });
    });
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND(std::ptr::null_mut()),
            -32000,
            -32000,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
        );
        let _ = ShowWindowAsync(hwnd, SW_HIDE);
    }
}
/// Make the window invisible without letting Explorer re-show it:
/// WS_EX_LAYERED + alpha 0, plus park it off-screen as a fallback.
///
/// The original screen rect and layered state are remembered, so a later
/// [`unhide`] (released window, failed merge, daemon shutdown) restores the
/// window exactly where it was instead of leaving it parked at -32000.
pub fn hide(hwnd: HWND) {
    let mut rect = RECT::default();
    let (layered, alpha) = unsafe {
        let _ = GetWindowRect(hwnd, &mut rect);
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let layered = (ex & WS_EX_LAYERED.0) != 0;
        (layered, if layered { alpha(hwnd).unwrap_or(255) } else { 255 })
    };
    let id = id_of(hwnd);
    // Only the first hide records the state: a second hide (watchdog after the
    // merger thread, or a re-hide) runs while the window is already parked at
    // -32000 and must not overwrite the real position.
    with_saved(|map| {
        map.entry(id).or_insert(Saved {
            rect: sane_rect(rect),
            layered,
            alpha,
        });
    });
    // Park it off-screen first: SetWindowPos costs ~0.6ms, while the first
    // SetWindowLongPtrW on a window owned by another (busy) thread blocks until
    // that thread pumps messages (~90-220ms). Doing the cheap move first means
    // the window can never be seen even if Explorer manages to ShowWindow() it
    // during that block.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND(std::ptr::null_mut()),
            -32000,
            -32000,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        if !layered {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex | WS_EX_LAYERED.0) as isize);
        }
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
    }
}
/// Bring a window to the foreground, restoring it if minimised.
///
/// After a merge the new tab lives in the main window; if that window is
/// behind other windows (or minimised) the user would see nothing at all after
/// pressing Win+E - the window must be raised the same way opening a folder
/// normally would.
pub fn activate_window(hwnd: HWND) {
    // A parked window would be "activated" while still off-screen: the user
    // sees the taskbar flash and nothing else. Put it back first.
    if is_offscreen(hwnd) || hidden_by_us(hwnd) {
        unhide(hwnd);
    }
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        // SwitchToThisWindow is the Alt+Tab style activation; it works even
        // when SetForegroundWindow is refused by the foreground lock.
        SwitchToThisWindow(hwnd, true);
        let _ = SetForegroundWindow(hwnd);
    }
}
/// Move a window to `rect` synchronously and make sure it is shown.
fn place(hwnd: HWND, rect: RECT) {
unsafe {
    let _ = SetWindowPos( hwnd, HWND(std::ptr::null_mut()), rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top, SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW, );
    let _ = ShowWindow(hwnd, SW_SHOWNA);
}
}
/// `(is on screen, is visible)` for a window.
fn on_screen(hwnd: HWND) -> (bool, bool) {
unsafe {
    let mut rect = RECT::default();
    let _ = GetWindowRect(hwnd, &mut rect);
    let off = rect.left <= -30_000 || rect.top <= -30_000;
    (!off, is_visible(hwnd))
}
}
/// Undo a late-arriving [`hide_async`] park.
///
/// `hide_async` posts a `SetWindowPos(-32000, -32000, SWP_ASYNCWINDOWPOS)` to the
/// Explorer thread and returns immediately. Explorer may still be busy creating
/// the window, so that queued move can land *after* a synchronous restore and
/// park the window off-screen again: the user gets "a window was opened but
/// nothing shows up on the desktop". Keep watch for a moment and repair.
fn settle_onscreen(hwnd: HWND, target: RECT) {
let deadline = Instant::now() + Duration::from_millis(150);
while Instant::now() < deadline {
    thread::sleep(Duration::from_millis(5));
    let (on, visible) = on_screen(hwnd);
    if !on || !visible {
        place(hwnd, target);
    }
}
}
/// Restore a window hidden by [`hide`] (keeps it in the background).
///
/// Windows hidden by a previous daemon process are not in `SAVED`; those get a
/// sane on-screen position instead of staying parked off-screen forever.
pub fn unhide(hwnd: HWND) {
    if !is_window(hwnd) {
        return;
    }
    let id = id_of(hwnd);
    let saved = with_saved(|map| map.remove(&id));
    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut rect);
    }
    let target = match saved {
        Some(s) => unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let ex2 = if s.layered { ex | WS_EX_LAYERED.0 } else { ex & !WS_EX_LAYERED.0 };
            if ex2 != ex {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex2 as isize);
            }
            if s.layered {
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), s.alpha, LWA_ALPHA);
            }
            sane_rect(s.rect)
        },
        None => unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex & !WS_EX_LAYERED.0) as isize);
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            // Unknown original position (window hidden by a previous process):
            // put it somewhere visible instead of leaving it parked at -32000.
            sane_rect(rect)
        },
    };

    place(hwnd, target);
    // Queue the same restore asynchronously so it is processed *after* a
    // still-pending park from hide_async (posted moves on one window queue are
    // processed in order), then verify the outcome and repair.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND(std::ptr::null_mut()),
            target.left,
            target.top,
            target.right - target.left,
            target.bottom - target.top,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS,
        );
    }
    settle_onscreen(hwnd, target);
}
/// Restore every Explorer window that is invisible on purpose: layered alpha 0
/// (hidden by us) or parked off-screen (hidden by an older build that did not
/// remember positions). Returns how many windows were touched.
///
/// New-style (WinUI3) windows and Explorer's pooled spare window are left
/// alone: the former break when moved, the latter is parked by Explorer on
/// purpose and must be kept where Explorer left it.
pub fn rescue_hidden() -> usize {
    let mut n = 0;
    for hwnd in cabinets() {
        if is_new_style_explorer(hwnd) {
            continue;
        }
        if parked_by_us(hwnd) {
            unhide(hwnd);
            n += 1;
        }
    }
    n
}
/// Explorer reports "access denied" / "location unavailable" with a modal
/// top-level #32770 dialog owned by the folder window. Such a window never
/// becomes a shell browser, so it is not a merge candidate.
pub fn has_owned_dialog(hwnd: HWND) -> bool {
    let cls = wide( "#32770" );
    let cls_ptr = PCWSTR(cls.as_ptr());
    let mut after = HWND(std::ptr::null_mut());
    loop {
        match
    unsafe {
            FindWindowExW(HWND(std::ptr::null_mut()), after, cls_ptr, PCWSTR(std::ptr::null()))
        } {
            Ok(h) if !h.0.is_null() => {
                let owner =
    unsafe {
                    GetWindow(h, GW_OWNER)
                };

                if owner == Ok(hwnd) {
                    return true;
                } after = h;
            } _ => return false,
        }
    }
}
/// DWMWA_CLOAK (13): undocumented but stable attribute used by the shell to
/// hide windows without touching their WS_VISIBLE state. DWM drops the window
/// from composition, so the user never sees it and Explorer's later
/// ShowWindow calls cannot bring it back.pub
const DWMWA_CLOAK: i32 = 13;
/// DWMWA_CLOAKED (14): read-only flag telling whether DWM considers the window
/// cloaked.pub
const DWMWA_CLOAKED: i32 = 14;
pub fn set_cloak(hwnd: HWND, on: bool) -> windows::core::Result<()> {
    let flag: i32 = i32::from(on);
unsafe {
        DwmSetWindowAttribute( hwnd, DWMWINDOWATTRIBUTE(DWMWA_CLOAK), &flag as *
const i32 as *
const core::ffi::c_void, 4, )
    }
}
pub fn get_cloak(hwnd: HWND) -> Option<i32> {
    let mut v: i32 = -1;
unsafe {
        DwmGetWindowAttribute( hwnd, DWMWINDOWATTRIBUTE(DWMWA_CLOAK), &mut v as *mut i32 as *mut core::ffi::c_void, 4, ) .ok()?
    };

    Some(v)
}
pub fn get_cloaked_flag(hwnd: HWND) -> Option<i32> {
    let mut v: i32 = -1;
unsafe {
        DwmGetWindowAttribute( hwnd, DWMWINDOWATTRIBUTE(DWMWA_CLOAKED), &mut v as *mut i32 as *mut core::ffi::c_void, 4, ) .ok()?
    };

    Some(v)
}
pub fn is_layered(hwnd: HWND) -> bool {
unsafe {
        (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_LAYERED.0) != 0
    }
}
/// Current layered alpha (0-255), if the window is layered at all.
pub fn alpha(hwnd: HWND) -> Option<u8> {
    let mut value: u8 = 255;
    let mut key = COLORREF(0);
    let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
    let got =
    unsafe {
        GetLayeredWindowAttributes(hwnd, Some(&mut key), Some(&mut value), Some(&mut flags))
    };

    got.ok().map(|_| value)
}
/// Our hide signature: layered with alpha 0 and parked far off-screen. Used to
/// clean up after a daemon that was killed mid-merge.
pub fn hidden_by_us(hwnd: HWND) -> bool {
    if !is_layered(hwnd) || alpha(hwnd) != Some(0) {
        return false;
    }
    let mut rect = RECT::default();
unsafe {
        let _ = GetWindowRect(hwnd, &mut rect);
    } rect.left <= -30_000
}
/// `ShowWindowAsync` never blocks: it posts the request to the owning thread.
pub fn show_async(hwnd: HWND, cmd: SHOW_WINDOW_CMD) -> bool {
unsafe {
        ShowWindowAsync(hwnd, cmd).as_bool()
    }
}
pub fn close(hwnd: HWND) {
unsafe {
        let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}
/// Magic command Ctrl+T: open a new tab in the window owning `tab_hwnd`.
pub fn magic_new_tab(tab_hwnd: HWND) {
unsafe {
        let _ = PostMessageW(tab_hwnd, WM_COMMAND, WPARAM(0xA21B), LPARAM(0));
    }
}
/// Magic command Ctrl+W-like tab close / detach (0xA021).
pub fn magic_tab_command(tab_hwnd: HWND, lparam: isize) {
unsafe {
        let _ = SendMessageW(tab_hwnd, WM_COMMAND, WPARAM(0xA021), LPARAM(lparam));
    }
}
/// Magic command Ctrl+1..n (0xA221) sent to the window.
pub fn magic_select_tab(window: HWND, one_based_index: i32) {
unsafe {
        let _ = SendMessageW( window, WM_COMMAND, WPARAM(0xA221), LPARAM(one_based_index as isize), );
    }
}
