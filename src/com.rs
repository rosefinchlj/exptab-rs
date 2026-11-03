//! COM plumbing: IShellWindows lookup and IWebBrowser2 helpers.

use windows::core::{Interface, IUnknown, VARIANT};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IDispatch, IServiceProvider, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    Folder, Folder2, FolderItem, IShellBrowser, IShellDispatch, IShellFolderViewDual,
    IWebBrowser2, IShellWindows, Shell, ShellWindowFindWindowOptions,
    ShellWindowTypeConstants, ShellWindows, SWC_BROWSER,
};

pub use windows::Win32::UI::Shell::SWFO_NEEDDISPATCH;

/// Convenience alias: every tab is represented by one browser object.
pub type Browser = IWebBrowser2;

#[derive(Clone, Debug)]
pub struct ShellEntry {
    pub hwnd: isize,
    pub url: String,
}

/// One tab: the per-tab `IWebBrowser2` plus the `ShellTabWindowClass` HWND it
/// belongs to (resolved through `IServiceProvider -> IShellBrowser::GetWindow`).
#[derive(Clone)]
pub struct TabEntry {
    pub tab_hwnd: isize,
    pub ie: IWebBrowser2,
    pub url: String,
}

pub fn init() -> windows::core::Result<()> {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }
}

pub fn shell_windows() -> windows::core::Result<IShellWindows> {
    unsafe {
        CoCreateInstance(
            &ShellWindows,
            None::<&windows::core::IUnknown>,
            CLSCTX_ALL,
        )
    }
}

/// HWND of the tab window that owns this browser object.
pub fn tab_hwnd_of(ie: &IWebBrowser2) -> Option<HWND> {
    let sp: IServiceProvider = ie.cast().ok()?;
    let sb: IShellBrowser = unsafe { sp.QueryService::<IShellBrowser>(&IShellBrowser::IID) }.ok()?;
    unsafe { sb.GetWindow().ok() }
}

/// Enumerate the shell windows collection, mapping every entry to its tab HWND.
pub fn tab_entries(sw: &IShellWindows) -> Vec<TabEntry> {
    let mut out = Vec::new();
    unsafe {
        let n = sw.Count().unwrap_or(0);
        for i in 0..n {
            if let Ok(disp) = sw.Item(&VARIANT::from(i)) {
                if let Ok(ie) = disp.cast::<IWebBrowser2>() {
                    let url = location_of(&ie);
                    let tab_hwnd = tab_hwnd_of(&ie).map(|h| h.0 as isize).unwrap_or(0);
                    out.push(TabEntry { tab_hwnd, ie, url });
                }
            }
        }
    }
    out
}

/// The browser object for one specific tab HWND (scan + match).
pub fn ie_for_tab(sw: &IShellWindows, tab: HWND) -> Option<IWebBrowser2> {
    tab_entries(sw)
        .into_iter()
        .find(|e| e.tab_hwnd == tab.0 as isize)
        .map(|e| e.ie)
}

/// `FindWindowSW` with non-null VARIANTs: (pvarloc=VT_EMPTY, pvarlocroot=VT_I4 hwnd).
pub fn find_window_sw(
    sw: &IShellWindows,
    hwnd: HWND,
    opts: ShellWindowFindWindowOptions,
) -> windows::core::Result<IWebBrowser2> {
    let loc = VARIANT::default();
    let root = VARIANT::from(hwnd.0 as isize as i32);
    let mut found_hwnd = 0i32;
    unsafe {
        let disp: IDispatch = sw.FindWindowSW(&loc, &root, SWC_BROWSER, &mut found_hwnd, opts)?;
        disp.cast()
    }
}

fn find_sw(
    sw: &IShellWindows,
    vloc: &VARIANT,
    vroot: &VARIANT,
    swclass: ShellWindowTypeConstants,
    opts: ShellWindowFindWindowOptions,
) -> windows::core::Result<IWebBrowser2> {
    let mut found_hwnd = 0i32;
    unsafe {
        let disp: IDispatch = sw.FindWindowSW(vloc, vroot, swclass, &mut found_hwnd, opts)?;
        disp.cast()
    }
}

/// Variant interpretation (b): (pvarloc=VT_I4 hwnd, pvarlocroot=VT_EMPTY).
pub fn find_window_sw_loc_first(
    sw: &IShellWindows,
    hwnd: HWND,
    opts: ShellWindowFindWindowOptions,
) -> windows::core::Result<IWebBrowser2> {
    let loc = VARIANT::from(hwnd.0 as isize as i32);
    let root = VARIANT::default();
    find_sw(sw, &loc, &root, SWC_BROWSER, opts)
}

/// Enumerate entries with each `IWebBrowser2::HWND()` (diagnostic only).
pub fn enumerate(sw: &IShellWindows) -> Vec<ShellEntry> {
    let mut out = Vec::new();
    unsafe {
        let n = sw.Count().unwrap_or(0);
        for i in 0..n {
            if let Ok(disp) = sw.Item(&VARIANT::from(i)) {
                if let Ok(ie) = disp.cast::<IWebBrowser2>() {
                    out.push(ShellEntry {
                        hwnd: ie.HWND().map(|h| h.0).unwrap_or(0),
                        url: ie
                            .LocationURL()
                            .map(|b| b.to_string())
                            .unwrap_or_default(),
                    });
                }
            }
        }
    }
    out
}

pub fn location_url(ie: &IWebBrowser2) -> Option<String> {
    unsafe { ie.LocationURL().ok().map(|b| b.to_string()) }
}

/// Location of a tab the way Explorer's own automation object reports it.
///
/// `LocationURL` is empty for virtual shell folders ("主文件夹"/Home, "此电脑"/This PC,
/// Recycle Bin, ...), because those have no file-system URL. Win+E opens exactly such a
/// window, so relying on `LocationURL` alone made those windows unmergeable. Fall back to
/// the shell namespace path (`::{CLSID}` / `shell:...`), which `Navigate2` accepts.
pub fn location_of(ie: &IWebBrowser2) -> String {
    if let Some(url) = location_url(ie) {
        if !url.trim().is_empty() {
            return url;
        }
    }
    unsafe { shell_namespace_path(ie).unwrap_or_default() }
}

unsafe fn shell_namespace_path(ie: &IWebBrowser2) -> Option<String> {
    let doc: IDispatch = ie.Document().ok()?;
    // `ShellFolderView` (the Document of an Explorer tab) implements IShellFolderViewDual.
    let view: IShellFolderViewDual = doc.cast().ok()?;
    let folder: Folder = view.Folder().ok()?;
    let folder2: Folder2 = folder.cast().ok()?;
    let item: FolderItem = folder2.Self_().ok()?;
    let path = item.Path().ok()?;
    Some(path.to_string())
}

pub fn ie_hwnd(ie: &IWebBrowser2) -> isize {
    unsafe { ie.HWND().map(|h| h.0).unwrap_or(0) }
}

pub fn busy(ie: &IWebBrowser2) -> bool {
    unsafe { ie.Busy().map(|b| b.as_bool()).unwrap_or(false) }
}

pub fn ready_state(ie: &IWebBrowser2) -> i32 {
    unsafe { ie.ReadyState().map(|r| r.0).unwrap_or(-1) }
}

/// `Navigate2` with a plain URL string (normal file-system folders).
pub fn navigate(ie: &IWebBrowser2, path: &str) -> windows::core::Result<()> {
    if needs_shell_namespace(path) {
        return navigate_to_namespace(ie, path);
    }
    let url = VARIANT::from(path);
    navigate2_raw(ie, &url)
}

/// Shell namespace paths (`::{CLSID}` like the Home folder, or paths containing
/// `#` / `%23`) cannot be navigated as a URL string: `Navigate2` rejects them
/// with E_INVALIDARG. The shell accepts a `Folder` automation object there
/// instead, exactly like `ExplorerTabUtility/Hooks/ExplorerWatcher.cs` does.
fn needs_shell_namespace(path: &str) -> bool {
    path.starts_with("::") || path.contains('#') || path.contains("%23")
}

fn navigate_to_namespace(ie: &IWebBrowser2, path: &str) -> windows::core::Result<()> {
    let shell: IShellDispatch = unsafe { CoCreateInstance(&Shell, None::<&IUnknown>, CLSCTX_ALL) }?;
    let folder: Folder = unsafe { shell.NameSpace(&VARIANT::from(path)) }?;
    let folder_disp: IUnknown = folder.cast()?;
    // VARIANT::from(IUnknown) yields a VT_UNKNOWN variant that owns one
    // reference; flip the type tag to VT_DISPATCH (9) so the browser accepts it
    // as a Folder automation object. Both tags store the same interface pointer
    // in the union, and VARIANT's `vt` field is the first two bytes (the type is
    // `#[repr(transparent)]` over the documented `VARIANT` layout), so the patch
    // is layout-safe; dropping the variant still releases the reference exactly
    // once (VariantClear handles VT_DISPATCH like VT_UNKNOWN).
    let mut var = VARIANT::from(folder_disp);
    unsafe {
        *(&mut var as *mut VARIANT as *mut u16) = 9; // VT_DISPATCH
    }
    navigate2_raw(ie, &var)
}

fn navigate2_raw(ie: &IWebBrowser2, url: &VARIANT) -> windows::core::Result<()> {
    // The IWebBrowser2 we get from IShellWindows is a cross-process proxy: the
    // marshaler rejects null pointers for the optional [in] VARIANT* arguments
    // (RPC_X_NULL_REF_POINTER). Pass non-null VT_EMPTY VARIANTs instead.
    let empty = VARIANT::default();
    let p = &empty as *const VARIANT;
    unsafe { ie.Navigate2(url, Some(p), Some(p), Some(p), Some(p)) }
}

pub fn stop(ie: &IWebBrowser2) {
    unsafe {
        let _ = ie.Stop();
    }
}

pub fn quit(ie: &IWebBrowser2) {
    unsafe {
        let _ = ie.Quit();
    }
}
