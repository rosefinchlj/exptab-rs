# 资源管理器标签合并 — Win32/COM 事实核查报告

> 目的：为 `exptab-rs`（Rust 版 ExplorerTabUtility）核实 A/B/C/D 四组 Win32/COM 事实，避免按 `docs/PLAN.md` 的假设盲写。
> 约束：本报告只做研究与读源码，**没有修改任何 Rust 源码，也没有运行 cargo build**。所有签名均逐条对照本机 cargo registry 里的 crate 源码；标注 ⚠ 的条目是推断或未实测。
> 日期：2026-11-02（本机时间，Asia/Shanghai）

证据等级标记：

| 标记 | 含义 |
|---|---|
| 【crate】 | 本机 `windows-0.58.0` / `windows-core-0.58.0` 源码，附 `文件:行号` |
| 【docs】 | Microsoft Learn 官方文档（URL） |
| 【refimpl】 | 第三方实现参考（ReactOS / C# 参考实现），附 `文件:行号` 或 URL |
| 【local】 | 本机环境实测/枚举（注册表、DLL 字符串、版本），附命令与结果摘要 |
| 【infer】 | 由上述证据推理得出，未经实测 |
| 【?】 | 未确认 |

---

## 0. 摘要：四个会导致返工的纠错点

| # | PLAN.md / 任务书里的假设 | 实际情况 | 依据 |
|---|---|---|---|
| 1 | `IShellWindows::FindWindowSW(hwnd)` 能"按 HWND 一次调用拿到 `IWebBrowser2`" | **完全不可能**。`FindWindowSW` **没有 HWND 入参**：`pvarLoc` 是 PIDL（或 cookie），`phwnd` 是**输出**参数 | 【crate】Shell:20889，【docs】MS Learn，【refimpl】ReactOS `WinList_FindFolderWindow` |
| 2 | `SWC_BROWSER = 4` | `SWC_BROWSER = 1`；`4` 是 `SWC_CALLBACK` | 【crate】Shell:27684-27691，【docs】同 URL |
| 3 | `CLSID_ShellWindows` 常量 | crate 里叫 `windows::Win32::UI::Shell::ShellWindows`（**无 `CLSID_` 前缀**） | 【crate】Shell:36922 |
| 4 | `C:\Windows\ExplorerFrame.dll` 在 Win11 不存在（Win10 路径已废） | **存在**，`10.0.26100.9949`，3.27 MB，且正是 `CLSID_ExecuteFolder` 的宿主、含 `ShellTabWindowClass`/`CabinetWClass`/`LaunchExplorerFlags`/`opennewtab` 字符串 | 【local】`Get-Item` + UTF-16 字符串扫描 |

同时有一个**正面发现**（可能直接改写合并链路，见 §D.1）：

> 本机存在 `HKCR\Folder\shell\opennewtab` 这个 shell verb（`DelegateExecute = {11dbb47c-a525-400b-9e80-a54615a090c0}`，即 `CLSID_ExecuteFolder`，`LaunchExplorerFlags = 96`），
> 即"在已有窗口的新标签页中打开该路径"是 Explorer **原生**能力，可用 `ShellExecuteExW(lpVerb="opennewtab")` 触发。
> 这比 PLAN 的 `0xA21B 建标签 + 轮询 + Navigate2` 少一次导航、少一轮轮询，且不需要 COM 交互。**目标窗口的选择规则未确认，见 §D.1.3。**

---

## 1. 环境与方法

### 1.1 本机实际路径（与任务书给的路径不同）

| 项 | 任务书写的 | 本机实际 |
|---|---|---|
| cargo home | `C:\Users\jun.liu\.cargo`（**不存在**） | `D:\Programs\Rust\cargo`（`$env:CARGO_HOME`） |
| windows 0.58 源码 | — | `D:\Programs\Rust\cargo\registry\src\rsproxy.cn-e3de039b2554c837\windows-0.58.0\` |
| windows-core 0.58 | — | `...\rsproxy.cn-e3de039b2554c837\windows-core-0.58.0\` |
| C# 参考实现 | `E:\use\ai\deepseek\Rust\ExplorerTabUtility` | 同（重点文件已逐行核对） |

下文用 `Shell:` / `Com:` / `Ole:` / `Acc:` / `WAM:` / `Found:` 简写指代：

```
Shell = R\windows-0.58.0\src\Windows\Win32\UI\Shell\mod.rs
Com   = R\windows-0.58.0\src\Windows\Win32\System\Com\mod.rs
Ole   = R\windows-0.58.0\src\Windows\Win32\System\Ole\mod.rs
Acc   = R\windows-0.58.0\src\Windows\Win32\UI\Accessibility\mod.rs
WAM   = R\windows-0.58.0\src\Windows\Win32\UI\WindowsAndMessaging\mod.rs
Found = R\windows-0.58.0\src\Windows\Win32\Foundation\mod.rs
R     = D:\Programs\Rust\cargo\registry\src\rsproxy.cn-e3de039b2554c837
```

### 1.2 本机 Explorer 相关环境

| 项 | 值 | 来源 |
|---|---|---|
| OS build | 26300（26H2，用户陈述） | 用户 |
| explorer.exe | 10.0.26100.1710 | 用户 |
| explorerframe.dll | 10.0.26100.9949，`C:\Windows\System32\explorerframe.dll`，3268608 字节 | 【local】 |
| 顶层窗口类 | `CabinetWClass` | 用户 / 【local】shell32+explorerframe 字符串 |
| 标签子窗口类 | `ShellTabWindowClass` | 同上（explorerframe.dll UTF-16 偏移 1355196） |

### 1.3 核对方式

- Rust 签名：在 registry 源码里 `Select-String` / 直接 `Get-Content` 读定义行与 impl 块，逐条抄录（含 `#[cfg(feature=...)]`）。
- COM 语义：MS Learn `exdisp.h` 文档 + ReactOS `shdocvw` 实现对照。
- 魔法命令：以 C# 实现为一手证据，另做 web 检索寻独立佐证（**未取得**，见 §C.4）。
- 注册表/DLL：本机枚举 + ASCII/UTF-16LE 双编码字符串扫描。

---

## A. `windows` crate 0.58 的精确 Rust 签名

### A.0 Cargo.toml（features 清单）

```toml
[dependencies]
windows = { version = "0.58", features = [
    "Win32_Foundation",                 # HWND / LPARAM / WPARAM / BOOL / HMODULE / COLORREF / SHANDLE_PTR
    "Win32_System_Com",                 # IShellWindows / IDispatch / IServiceProvider / CoCreateInstance / CoInitializeEx / CoTaskMemFree / IConnectionPoint*
    "Win32_System_Ole",                 # IShellBrowser 的 Deref 目标 IOleWindow::GetWindow、READYSTATE
    "Win32_System_Registry",            # SHELLEXECUTEINFOW 含 HKEY 字段，整个结构体被此 feature 门控（Shell:36142）
    "Win32_UI_Shell",                   # IShellWindows / IWebBrowser2 / IShellBrowser / SIGDN / SHParseDisplayName / IExecuteCommand / IShellItem*
    "Win32_UI_Shell_Common",            # ITEMIDLIST（`Win32::UI::Shell::Common::ITEMIDLIST`）
    "Win32_UI_WindowsAndMessaging",     # EVENT_OBJECT_* / WM_COMMAND / FindWindowExW / PostMessageW / SetWindowPos ...
    "Win32_UI_Accessibility",           # SetWinEventHook / WINEVENTPROC / HWINEVENTHOOK
] }
```

feature 依赖关系（`R\windows-0.58.0\Cargo.toml`）：全部是扁平的 `X = ["父"]`，**不会自动带上兄弟 feature**，但方法级 `#[cfg(feature=...)]` 要求显式开启，漏了会出现"接口存在但方法不存在"的诡异报错。典型：`IShellWindows` 的 `Item/Count/FindWindowSW` 整个 impl 块被 `#[cfg(feature = "Win32_System_Com")]` 包住（Shell:20837-20838），`IShellBrowser` 的 `GetWindow` 来自 `Ole::IOleWindow`，缺 `Win32_System_Ole` 就没有 `GetWindow`。

> 注意：`SHELLEXECUTEINFOW` 被 `#[cfg(feature = "Win32_System_Registry")]` 门控（Shell:36142），这是"用 ShellExecuteEx 还得开 Registry feature"的唯一原因。若不想开，可自己 `#[repr(C)]` 定义同构结构体，但没必要。

### A.1 模块归属总表

| 项 | Rust 路径 | 需要的 feature | 源码位置 |
|---|---|---|---|
| `IShellWindows` | `windows::Win32::UI::Shell::IShellWindows`（IID `85cb6900-4d95-11cf-960c-0080c7f4ee85`） | Win32_UI_Shell + Win32_System_Com | Shell:20836 |
| `CLSID_ShellWindows` | `windows::Win32::UI::Shell::ShellWindows : GUID` | Win32_UI_Shell | Shell:36922 |
| `IWebBrowser2` | `windows::Win32::UI::Shell::IWebBrowser2`（`d30c1661-cdaf-11d0-8a3e-00c04fc9e26e`） | Win32_UI_Shell + Win32_System_Com | Shell:23973 |
| `IWebBrowserApp` | 同模块（`0002df05-0000-0000-c000-000000000046`） | 同上 | Shell:24110 |
| `IWebBrowser` | 同模块（`eab22ac1-30c1-11cf-a7eb-0000c05bae0b`） | 同上 | Shell:23822 |
| `IServiceProvider` | `windows::Win32::System::Com::IServiceProvider` | Win32_System_Com | Com:3754 |
| `IShellBrowser` | `windows::Win32::UI::Shell::IShellBrowser`（`000214e2-0000-0000-c000-000000000046`） | Win32_UI_Shell + **Win32_System_Ole** | Shell:16921 |
| `SID_STopLevelBrowser` | `windows::Win32::UI::Shell::SID_STopLevelBrowser` = `4c96be40-915c-11cf-99d3-00aa004ae837` | Win32_UI_Shell | Shell:27268 |
| `IShellView` | 同模块（`000214e3-...`） | Win32_UI_Shell + Ole | Shell:20662 |
| `DShellWindowsEvents` | 同模块（`fe4106e0-399a-11d0-a48c-00a0c90a8f39`），**vtbl 只有 IDispatch 基类、无类型化方法** | Win32_UI_Shell + Com | Shell:5740-5759 |
| `IConnectionPointContainer` / `IConnectionPoint` | `windows::Win32::System::Com::*` | Win32_System_Com | Com:1865 / Com:1824 |
| `SetWinEventHook` / `WINEVENTPROC` | `windows::Win32::UI::Accessibility::*` | Win32_UI_Accessibility | Acc:309 / Acc:10217 |
| `EVENT_OBJECT_*` / `WM_COMMAND` / `FindWindowExW` / `PostMessageW` / `SendMessageW` / `SetWindowPos` / `SetLayeredWindowAttributes` / `GetWindowLongPtrW` / `SetWindowLongPtrW` | `windows::Win32::UI::WindowsAndMessaging::*` | Win32_UI_WindowsAndMessaging | 见 A.6 |
| `IExecuteCommand` | `windows::Win32::UI::Shell::IExecuteCommand`（`7f9185b0-cb92-43c5-80a9-92277...`） | Win32_UI_Shell | Shell:10701 |
| `IShellItemArray` / `IObjectWithSelection` / `SHCreateItemFromParsingName` | 同模块 | Win32_UI_Shell | Shell:18780 / 15294 / 2783 |

### A.2 `IShellWindows`（重点：`FindWindowSW` 的真实形状）

```rust
// Shell:20836
define_interface!(IShellWindows, IShellWindows_Vtbl, 0x85cb6900_4d95_11cf_960c_0080c7f4ee85);
// Deref -> windows::Win32::System::Com::IDispatch；impl 块整体 #[cfg(feature = "Win32_System_Com")]
```

| 方法 | 精确签名 | 行号 |
|---|---|---|
| `Item` | `pub unsafe fn Item<P0>(&self, index: P0) -> Result<Com::IDispatch> where P0: Param<VARIANT>` | Shell:20853 |
| `Count` | `pub unsafe fn Count(&self) -> Result<i32>` | Shell:20860 |
| `Register` | `pub unsafe fn Register<P0>(&self, pid: P0, hwnd: i32, swclass: ShellWindowTypeConstants) -> Result<i32> where P0: Param<IDispatch>` | Shell:20863 |
| `RegisterPending` | `pub unsafe fn RegisterPending(&self, lthreadid: i32, pvarloc: *const VARIANT, pvarlocroot: *const VARIANT, swclass: ShellWindowTypeConstants, phwnd: *mut i32) -> Result<i32>` ⚠（参数按行内截断抄录，仅作参考） | Shell:20872 |
| `Revoke` | `pub unsafe fn Revoke(&self, lcookie: i32) -> Result<()>` | — |
| `OnNavigate` | `pub unsafe fn OnNavigate(&self, lcookie: i32, pvarloc: *const VARIANT) -> Result<()>` | — |
| **`FindWindowSW`** | 见下 | **Shell:20889** |

```rust
// Shell:20889 —— 注意：没有 HWND 入参；phwnd 是 *mut i32（输出）
pub unsafe fn FindWindowSW(
    &self,
    pvarloc: *const windows_core::VARIANT,       // in: PIDL（或 cookie）
    pvarlocroot: *const windows_core::VARIANT,   // in: NULL 或 VT_EMPTY
    swclass: ShellWindowTypeConstants,
    phwnd: *mut i32,                             // OUT: 匹配窗口的 HWND，LONG 类型
    swfwoptions: ShellWindowFindWindowOptions,
) -> windows_core::Result<windows::Win32::System::Com::IDispatch>;
```

常量（Shell:27684-27691）：

```rust
ShellWindowTypeConstants: SWC_EXPLORER = 0, SWC_BROWSER = 1, SWC_3RDPARTY = 2,
                           SWC_CALLBACK = 4, SWC_DESKTOP = 8
ShellWindowFindWindowOptions: SWFO_NEEDDISPATCH = 1, SWFO_INCLUDEPENDING = 2, SWFO_COOKIEPASSED = 4
```

> **`SWC_BROWSER` 是 `1`，不是 `4`。** 任务书 / PLAN 里的 `SWC_BROWSER=4` 是错的（4 是 `SWC_CALLBACK`，桌面窗口用）。
> 命名上 crate 用 `ShellWindowTypeConstants(1)` 这种 newtype，传常量即可，不要传裸 `i32`。

### A.3 `IWebBrowser2` 家族（`Win32::UI::Shell`，**不是** `Win32_Web_InternetExplorer`）

继承链：`IUnknown → IDispatch → IWebBrowser → IWebBrowserApp → IWebBrowser2`（`Deref` 逐级，所以 `IWebBrowser2` 上可直接调父接口方法）。

```rust
// Shell:23973
define_interface!(IWebBrowser2, IWebBrowser2_Vtbl, 0xd30c1661_cdaf_11d0_8a3e_00c04fc9e26e);
```

| 方法 | 精确签名 | 行号 | 备注 |
|---|---|---|---|
| `Navigate2` | `pub unsafe fn Navigate2(&self, url: *const VARIANT, flags: Option<*const VARIANT>, targetframename: Option<*const VARIANT>, postdata: Option<*const VARIANT>, headers: Option<*const VARIANT>) -> Result<()>` | Shell:23985 | `url` 是**裸指针**，要传 `&v as *const _`；后四个传 `None` |
| `Quit` | `pub unsafe fn Quit(&self) -> Result<()>` | Shell:24122 | 关的是**该浏览器对象所属窗口** |
| `PutProperty` | `pub unsafe fn PutProperty<P0, P1>(&self, property: P0, vtvalue: P1) -> Result<()> where P0: Param<BSTR>, P1: Param<VARIANT>` | Shell:24128 | C# 用它打 `seenBefore` 标记，Rust 可用 `HashSet` 代替 |
| `GetProperty` | `pub unsafe fn GetProperty<P0>(&self, property: P0) -> Result<VARIANT> where P0: Param<BSTR>` | Shell:24135 | |
| `HWND` | `pub unsafe fn HWND(&self) -> Result<Foundation::SHANDLE_PTR>` | Shell:24146 | `SHANDLE_PTR(pub isize)`（Found:11010）。⚠ 标签场景返回值语义未确认，见 §B.4 |
| `ReadyState` | `pub unsafe fn ReadyState(&self) -> Result<Ole::READYSTATE>` | Shell:24001 | 需 `Win32_System_Ole`；`READYSTATE_COMPLETE = READYSTATE(4)`（Ole:8121）、`READYSTATE_LOADING = READYSTATE(1)`（Ole:8124） |
| `Stop` | `pub unsafe fn Stop(&self) -> Result<()>` | Shell:23858 | 来自 `IWebBrowser` |
| `Document` | `pub unsafe fn Document(&self) -> Result<Com::IDispatch>` | Shell:23877 | 非文件系统命名空间的兜底入口，见 §A.3.1 |
| `LocationURL` | `pub unsafe fn LocationURL(&self) -> Result<BSTR>` | Shell:23921 | `BSTR` → `String` 用 `to_string()` |
| `LocationName` | `pub unsafe fn LocationName(&self) -> Result<BSTR>` | Shell:23917 | 显示名，非路径 |
| `Refresh` / `Refresh2` | `Refresh(&self) -> Result<()>` / `Refresh2(&self, level: Option<*const VARIANT>) -> Result<()>` | Shell:23852 / 23855 | |
| `Navigate` | `pub unsafe fn Navigate<P0>(&self, url: P0, flags: Option<*const VARIANT>, targetframename: Option<*const VARIANT>, postdata: Option<*const VARIANT>, headers: Option<*const VARIANT>) -> Result<()> where P0: Param<BSTR>` | Shell:23846 | 参数是 BSTR（不是 VARIANT） |
| `GoBack` / `GoForward` | `-> Result<()>` | Shell:23834 / 23837 | |

#### A.3.1 非文件系统位置（回收站 / 此电脑 / 控制面板）取路径

C# 的兜底（`ExplorerWatcher.cs:774-782`）：

```csharp
path = ((window.Document as ShellFolderView)!.Folder as Folder2)!.Self.Path;
```

Rust 等价链（全部在本机 crate 中，逐条核对过）：

```rust
// IWebBrowser2::Document() -> IDispatch
//   -> IShellFolderViewDual (Shell:18030)   .Folder()  -> Folder (Shell:18053)
//   -> cast::<Folder2>() (Shell:5898)       .Self_()   -> FolderItem (Shell:6007/5975)
//   -> .Path() -> BSTR
unsafe fn location_fallback(browser: &IWebBrowser2) -> windows::core::Result<String> {
    use windows::core::Interface;
    let doc: Com::IDispatch = browser.Document()?;
    let fv: IShellFolderViewDual = doc.cast()?;
    let folder: Folder = fv.Folder()?;
    let f2: Folder2 = folder.cast()?;         // Folder2 的 Self_ 不在 Folder 上
    let item: FolderItem = f2.Self_()?;
    Ok(item.Path()?.to_string())
}
```

> 这个链比 PLAN 里写的 `IShellBrowser → IShellView → IPersistFolder2::GetCurFolder → SHGetNameFromIDList` 少两次 QI，且与 C# 行为一致，建议优先实现。`IPersistFolder2` 确实也在 crate 里（Shell:15700），留作二线兜底。

### A.4 `VARIANT` 的构造（`windows::core::VARIANT`）

实现位置：`windows-core-0.58.0\src\variant.rs`，通过 `windows-core\src\windows.rs:15-18` 的 `mod variant; pub use variant::*;` 在 `#[cfg(windows)]` 下导出；`windows::core` = `windows_core`（`windows-0.58.0\src\lib.rs` 有 `pub use windows_core as core;`）。

**有**这些 `From`：

| 转换 | 结果 vt | 行号 |
|---|---|---|
| `VARIANT::from(i32)` | `VT_I4` | variant.rs:622 |
| `VARIANT::from(&str)` / `from(String)` | `VT_BSTR`（内部 `BSTR::from`） | variant.rs:319 |
| `VARIANT::from(BSTR)` | `VT_BSTR` | variant.rs:283 |
| `VARIANT::from(bool)` | `VT_BOOL` | variant.rs:350 |
| `VARIANT::from(IUnknown)` | `VT_UNKNOWN` | variant.rs:213 |

**没有** `From<u32>` / `From<usize>` / `From<HWND>`——要 I4 就先 `as i32`。

其它要点：

- `VARIANT::new()` = `VT_EMPTY`（variant.rs:139）；`is_empty()`（144）；`unsafe fn from_raw(imp::VARIANT)`（153）；`fn as_raw(&self) -> &imp::VARIANT`（158）。
- **`impl Drop for VARIANT` 会调用 `imp::VariantClear`**（variant.rs:44-46）→ 持有 BSTR 的 VARIANT 不会泄漏，不需要手动 `VariantClear`。
- `Param<VARIANT>` 由 `impl<T> Param<T, CloneType> for &T` 提供（`windows-core\src\param.rs:45`）→ 传 `&VARIANT` 即可；`Param<BSTR>` 同理。

**手工构造（推荐用 `From`，这里给出字段路径以备特殊 vt）**：`windows_core::imp` 是 `#[doc(hidden)] pub mod imp`（`windows-core\src\lib.rs:22-23`），可直达：

```rust
use windows::core::{imp, VARIANT};

/// VT_I4 手搓版（一般直接用 VARIANT::from(v: i32)）
fn variant_i4(v: i32) -> VARIANT {
    unsafe {
        VARIANT::from_raw(imp::VARIANT {
            Anonymous: imp::VARIANT_0 {
                Anonymous: imp::VARIANT_0_0 {
                    vt: imp::VT_I4,          // VT_I4 = 3u16，imp::VARENUM = u16
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: imp::VARIANT_0_0_0 { lVal: v },
                },
            },
        })
    }
}

/// VT_BSTR 手搓版（一般直接用 VARIANT::from("...")）
fn variant_bstr(s: &str) -> VARIANT {
    // From<&str> 就是 BSTR::from(value).into()，内部走同一 union 字段 bstrVal
    VARIANT::from(s)
}
```

- union 字段名（`windows-core\src\imp\bindings.rs:575-648`）：`VARIANT_0_0_0 { llVal, lVal, bVal, iVal, fltVal, dblVal, boolVal, bstrVal, punkVal, pdispVal, ... }`。
- vt 常量：`VT_EMPTY = 0u16, VT_I4 = 3u16, VT_BSTR = 8u16`（bindings.rs:657-669）；类型 `pub type VARENUM = u16`。
- 如果真要构造带 PIDL 的 VARIANT 给 `FindWindowSW`：文档要求 `VT_VARIANT | VT_BYREF`（`0x400C`），ReactOS 实现里用的是 `VT_ARRAY | VT_UI1` 的 SafeArray（见 §B.2）。**两者都需要先有 PIDL，见 §B 结论：我们不需要它。**

### A.5 COM 初始化与对象创建

```rust
// Com:314
pub unsafe fn CoInitializeEx(pvreserved: Option<*const core::ffi::c_void>, dwcoinit: COINIT) -> HRESULT;
// Com:4892
pub const COINIT_APARTMENTTHREADED: COINIT = COINIT(2i32);
// Com:563
pub unsafe fn CoUninitialize();   // -> ()

// Com:105
pub unsafe fn CoCreateInstance<P0, T>(
    rclsid: *const GUID,
    punkouter: P0,          // Param<IUnknown>
    dwclscontext: CLSCTX,
) -> Result<T> where P0: Param<IUnknown>, T: Interface;

// Com:4865 / Com:4875
pub const CLSCTX_ALL: CLSCTX = CLSCTX(23u32);
pub const CLSCTX_INPROC_SERVER: CLSCTX = CLSCTX(1u32);

// Found:9418-9419
pub const S_OK: HRESULT = HRESULT(0x0);
pub const S_FALSE: HRESULT = HRESULT(0x1);
```

需要 `CoInitializeEx(None, COINIT_APARTMENTTHREADED)` 吗？**需要，而且必须是 STA**：

1. `CLSID_ShellWindows` 的类厂与 `IShellBrowser`/`IOleWindow` 都是 apartment 语义；`CoCreateInstance` 返回的代理在 MTA 里调 `QueryService`/`GetWindow` 会变成跨 apartment 编组，慢且可能失败。
2. WinEvent 回调要在**安装 hook 的线程**上有消息循环（`SetWinEventHook` out-of-context 的投递依赖该线程 pump 消息）。STA + `GetMessage` 循环正好满足。
3. `CLSCTX`：用 `CLSCTX_ALL`（=23）。`CLSID_ShellWindows` 是 LocalServer（explorer.exe 内），`CLSCTX_INPROC_SERVER` 会失败（E_ACCESSDENIED/REGDB_E_CLASSNOTREG 类）。**不要**用 `CLSCTX_INPROC_SERVER`。

`CoInitializeEx` 返回 `S_FALSE` 表示"本线程已初始化过"，此时**不要** `CoUninitialize`；只有 `S_OK` 才配对。参考实现：

```rust
struct Apartment { uninit: bool }

impl Apartment {
    fn init_sta() -> windows::core::Result<Self> {
        let hr = unsafe { Com::CoInitializeEx(None, Com::COINIT_APARTMENTTHREADED) };
        hr.ok()?;                       // S_FALSE 也算 Ok
        Ok(Self { uninit: hr == Found::S_OK })
    }
}
impl Drop for Apartment {
    fn drop(&mut self) { if self.uninit { unsafe { Com::CoUninitialize() } } }
}
```

### A.6 窗口 / 消息 / WinEvent 精确签名

| API | 精确签名 | 行号 |
|---|---|---|
| `SetWinEventHook` | `pub unsafe fn SetWinEventHook<P0>(eventmin: u32, eventmax: u32, hmodwineventproc: P0, pfnwineventproc: WINEVENTPROC, idprocess: u32, idthread: u32, dwflags: u32) -> HWINEVENTHOOK where P0: Param<HMODULE>` | Acc:309 |
| `WINEVENTPROC` | `pub type WINEVENTPROC = Option<unsafe extern "system" fn(hwineventhook: HWINEVENTHOOK, event: u32, hwnd: HWND, idobject: i32, idchild: i32, ideventthread: u32, dwmseventtime: u32)>` | Acc:10217 |
| `UnhookWinEvent` | `pub unsafe fn UnhookWinEvent<P0>(hwineventhook: P0) -> BOOL where P0: Param<HWINEVENTHOOK>` | Acc:910 |
| `HWINEVENTHOOK` | `pub struct HWINEVENTHOOK(pub *mut c_void)` | Acc:9678 |
| `FindWindowExW` | `pub unsafe fn FindWindowExW<P0,P1,P2,P3>(hwndparent: P0, hwndchildafter: P1, lpszclass: P2, lpszwindow: P3) -> Result<HWND>` | WAM:924 |
| `GetClassNameW` | `pub unsafe fn GetClassNameW<P0>(hwnd: P0, lpclassname: &mut [u16]) -> i32` | WAM:1076 |
| `PostMessageW` | `pub unsafe fn PostMessageW<P0,P1,P2>(hwnd: P0, msg: u32, wparam: P1, lparam: P2) -> Result<()>` | WAM:2434 |
| `SendMessageW` | `pub unsafe fn SendMessageW<P0,P1,P2>(hwnd: P0, msg: u32, wparam: P1, lparam: P2) -> LRESULT` | WAM:2709 |
| `SetWindowPos` | `pub unsafe fn SetWindowPos<P0,P1>(hwnd: P0, hwndinsertafter: P1, x: i32, y: i32, cx: i32, cy: i32, uflags: SET_WINDOW_POS_FLAGS) -> Result<()>` | WAM:3050 |
| `SetLayeredWindowAttributes` | `pub unsafe fn SetLayeredWindowAttributes<P0,P1>(hwnd: P0, crkey: P1, balpha: u8, dwflags: LAYERED_WINDOW_ATTRIBUTES_FLAGS) -> Result<()>` | WAM:2860 |
| `GetWindowLongPtrW` | `pub unsafe fn GetWindowLongPtrW<P0>(hwnd: P0, nindex: WINDOW_LONG_PTR_INDEX) -> isize` | WAM:1551 |
| `SetWindowLongPtrW` | `pub unsafe fn SetWindowLongPtrW<P0>(hwnd: P0, nindex: WINDOW_LONG_PTR_INDEX, dwnewlong: isize) -> isize` | WAM:3026 |
| `ShowWindow` | `pub unsafe fn ShowWindow<P0>(hwnd: P0, ncmdshow: SHOW_WINDOW_CMD) -> BOOL` | WAM:3138 |
| `EnumWindows` | `pub unsafe fn EnumWindows<P0>(lpenumfunc: WNDENUMPROC, lparam: P0) -> Result<()>` | WAM:894 |
| `IsWindow` / `IsWindowVisible` | `-> BOOL` | WAM:1834 / 1858 |
| `SetForegroundWindow` | `pub unsafe fn SetForegroundWindow<P0>(hwnd: P0) -> BOOL` | WAM:2852 |
| `GetWindowThreadProcessId` | `pub unsafe fn GetWindowThreadProcessId<P0>(hwnd: P0, lpdwprocessid: Option<*mut u32>) -> u32` | WAM:1631 |
| `ShellExecuteExW` | `pub unsafe fn ShellExecuteExW(pexecinfo: *mut SHELLEXECUTEINFOW) -> Result<()>` | Shell:4590 |

常量：

| 常量 | 值 | 类型 | 行号 |
|---|---|---|---|
| `EVENT_OBJECT_CREATE` | `32768u32` (0x8000) | u32 | WAM:3676 |
| `EVENT_OBJECT_SHOW` | `32770u32` (0x8002) | u32 | WAM:3704 |
| `EVENT_OBJECT_DESTROY` / `HIDE` / `FOCUS` / `REORDER` | 32769 / 32771 / 32773 / 32772 | u32 | WAM:3679-3699 |
| `WINEVENT_OUTOFCONTEXT` / `WINEVENT_SKIPOWNPROCESS` / `WINEVENT_SKIPOWNTHREAD` | 0 / 2 / 1 | u32 | WAM:5220-5222 |
| `WM_COMMAND` / `WM_CLOSE` | 273 / 16 | u32 | WAM:5259 / 5258 |
| `GWL_EXSTYLE` | `WINDOW_LONG_PTR_INDEX(-20)` | WINDOW_LONG_PTR_INDEX | WAM:3855 |
| `WS_EX_LAYERED` | `WINDOW_EX_STYLE(0x80000)` | WINDOW_EX_STYLE | WAM:5516 |
| `LWA_ALPHA` | `LAYERED_WINDOW_ATTRIBUTES_FLAGS(2)` | LAYERED_WINDOW_ATTRIBUTES_FLAGS | WAM:4169 |
| `SWP_HIDEWINDOW`/`SWP_NOSIZE`/`SWP_NOZORDER`/`SWP_NOACTIVATE`/`SWP_FRAMECHANGED`/`SWP_SHOWWINDOW` | 0x80 / 1 / 4 / 16 / 32 / 64 | SET_WINDOW_POS_FLAGS | WAM:5093/5101/5102/5094/5092/5103 |
| `SIGDN_FILESYSPATH` / `SIGDN_URL` | `SIGDN(-2147123200)` / `SIGDN(-2147057664)` | SIGDN | Shell:27274 / 27281 |

> **重要：`EVENT_OBJECT_*` 常量在 `Win32::UI::WindowsAndMessaging`，不在 `Win32::UI::Accessibility`**（后者只有 `SetWinEventHook`/`WINEVENTPROC`/`HWINEVENTHOOK`）。写错模块会得到"常量不存在"的编译错误。

引用类型（Found）：

```rust
HWND(pub *mut c_void)      // :10866
LPARAM(pub isize)          // :10888
LRESULT(pub isize)         // :10899
WPARAM(pub usize)          // :11081
HMODULE(pub *mut c_void)   // :10772
COLORREF(pub u32)          // :10532
BOOL(pub i32)              // :10510
SHANDLE_PTR(pub isize)     // :11010
```

`Param` 的四个 blanket 实现（`param.rs:16/28/45/54`）决定了怎么传参：

- `Option<&T>` → 可以传 `None::<&HWND>`、`None::<&HMODULE>`（必须带 turbofish 标注，否则 `None` 无法推断）；
- `&U`（U: Interface）→ 传 `&shell_windows` 作 `Param<IUnknown>`；
- `&T`（CloneType）→ 传 `&VARIANT`、`&BSTR`；
- `U`（CopyType，`U: CanInto<T>`）→ 直接传值：`HWND(ptr)`、`WPARAM(0xA21B)`、`LPARAM(1)`、`COLORREF(0)`。

### A.7 可直接使用的片段

> 说明：以下代码**按上述签名手写、未跑 cargo build**（任务限定只写报告）。逻辑与 C# 参考实现一一对应，方便对照。`⚠` 处见 §B/§D 的未确认项。

**(1) STA + 创建 `IShellWindows`**

```rust
use windows::core::{Interface, IUnknown, Result, VARIANT};
use windows::Win32::Foundation::{HWND, S_FALSE, S_OK};
use windows::Win32::System::Com as ComApp;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::UI::Shell::{IShellWindows, ShellWindows};

fn create_shell_windows() -> Result<IShellWindows> {
    unsafe { CoCreateInstance(&ShellWindows, None::<&IUnknown>, CLSCTX_ALL) }
}
```

**(2) 枚举 ShellWindows → `(标签 HWND, IWebBrowser2)` 映射（替代不存在的 `FindWindowSW(hwnd)`）**

```rust
use windows::Win32::System::Com::IServiceProvider;
use windows::Win32::UI::Shell::{IShellBrowser, IWebBrowser2, SID_STopLevelBrowser};

pub struct TabEntry { pub tab_hwnd: HWND, pub browser: IWebBrowser2 }

pub fn enumerate_tabs(sw: &IShellWindows) -> Result<Vec<TabEntry>> {
    let mut out = Vec::new();
    let count = unsafe { sw.Count()? };
    for i in 0..count {
        let idx = VARIANT::from(i);                        // VT_I4
        let Ok(disp) = (unsafe { sw.Item(&idx) }) else { continue };
        let Ok(browser) = disp.cast::<IWebBrowser2>() else { continue };
        let Ok(sp) = browser.cast::<IServiceProvider>() else { continue };
        // guidService 用 SID_STopLevelBrowser 是文档语义；C# 实测用 IShellBrowser::IID 也能work（见 §B.3）
        let sb: IShellBrowser = match unsafe { sp.QueryService(&SID_STopLevelBrowser) } {
            Ok(v) => v,
            Err(_) => match unsafe { sp.QueryService(&IShellBrowser::IID) } {
                Ok(v) => v,
                Err(_) => continue,
            },
        };
        let Ok(hwnd) = (unsafe { sb.GetWindow() }) else { continue };  // 来自 Ole::IOleWindow
        out.push(TabEntry { tab_hwnd: hwnd, browser });
    }
    Ok(out)
}
```

**(3) 读路径 / 导航 / 中止 / 关窗**

```rust
pub fn location_url(b: &IWebBrowser2) -> Option<String> {
    unsafe { b.LocationURL().ok() }.map(|s| s.to_string())
}

pub fn navigate2(b: &IWebBrowser2, path: &str) -> Result<()> {
    let url = VARIANT::from(path);                         // VT_BSTR
    unsafe { b.Navigate2(&url as *const VARIANT, None, None, None, None) }
}

pub fn stop(b: &IWebBrowser2) { let _ = unsafe { b.Stop() }; }
pub fn quit(b: &IWebBrowser2) { let _ = unsafe { b.Quit() }; }
```

**(4) 三条已确认的魔法命令（详见 §C）**

```rust
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, PostMessageW, SendMessageW, WM_COMMAND,
};
use windows::core::w;

const CMD_NEW_TAB:    usize = 0xA21B;   // 发给 ShellTabWindowClass 子窗口，Post
const CMD_SELECT_TAB: usize = 0xA221;   // 发给 CabinetWClass，lParam = 索引+1
const CMD_CLOSE_TAB:  usize = 0xA021;   // 发给活动标签 hwnd，lParam = 1

pub fn new_tab(window: HWND) -> Result<()> {
    // 活动标签永远在 z 序第一位（C# ExplorerWatcher.cs:739 同款做法）
    let tab = unsafe {
        FindWindowExW(window, HWND(std::ptr::null_mut()), w!("ShellTabWindowClass"), None)?
    };
    unsafe { PostMessageW(tab, WM_COMMAND, WPARAM(CMD_NEW_TAB), LPARAM(0)) }
}

pub fn select_tab(window: HWND, index: usize) {
    unsafe { SendMessageW(window, WM_COMMAND, WPARAM(CMD_SELECT_TAB), LPARAM((index + 1) as isize)) };
}

pub fn close_tab(tab: HWND) {
    unsafe { SendMessageW(tab, WM_COMMAND, WPARAM(CMD_CLOSE_TAB), LPARAM(1)) };
}
```

**(5) 隐藏 / 还原（对齐 C# `Helper.cs:385-423`）**

```rust
use windows::Win32::Foundation::COLORREF;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos,
    GWL_EXSTYLE, LWA_ALPHA, SWP_FRAMECHANGED, SWP_HIDEWINDOW, SWP_NOACTIVATE,
    SWP_NOZORDER, SWP_NOSIZE, WS_EX_LAYERED,
};

pub fn hide_layered(hwnd: HWND) -> Result<()> {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA)   // alpha = 0
    }
}

pub fn show_layered(hwnd: HWND) -> Result<()> {
    unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA) }
}

/// 主题切换时分层窗口可能失效的兜底（C# 的 "keepTheme" 分支）
pub fn hide_offscreen(hwnd: HWND) -> Result<()> {
    unsafe {
        SetWindowPos(hwnd, None::<&HWND>, -32000, -32000, 0, 0,
            SWP_HIDEWINDOW | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED)
    }
}
```

**(6) WinEvent hook（CREATE + SHOW 双事件）**

```rust
use windows::Win32::Foundation::HMODULE;
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK, WINEVENTPROC};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

unsafe extern "system" fn on_win_event(
    _hook: HWINEVENTHOOK, event: u32, hwnd: HWND,
    id_object: i32, id_child: i32, _thread: u32, _time: u32,
) {
    if id_object != 0 || id_child != 0 { return; }        // 只要窗口本身
    if hwnd.0.is_null() { return; }
    // event == EVENT_OBJECT_CREATE / EVENT_OBJECT_SHOW 分派；class 过滤用 GetClassNameW
    let _ = event;
}

pub fn install_hook() -> HWINEVENTHOOK {
    unsafe {
        SetWinEventHook(
            EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW,
            None::<&HMODULE>, Some(on_win_event),
            0, 0,                                        // 全进程 / 全线程
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    }
}
```

> hook 返回值是 `HWINEVENTHOOK(*mut c_void)`，失败为 null，**不是 Result**，必须自己判 `hook.0.is_null()`。
> 回调在安装线程（有消息循环的那个 STA 线程）上被调用 → 不要在回调里做阻塞等待，只投递到队列。

---

## B. `FindWindowSW` 对"标签级 HWND"是否有效

### B.1 结论

**无效，且方向性错误。** `IShellWindows::FindWindowSW` 是"按**位置**（PIDL/cookie）找窗口"，不是"按 HWND 找浏览器对象"。它没有 HWND 入参，`phwnd` 是**输出**。

### B.2 三份独立证据

1. **crate 签名**（Shell:20889）：参数是 `pvarloc: *const VARIANT`、`pvarlocroot: *const VARIANT`、`swclass`、`phwnd: *mut i32`、`swfwoptions`。没有 HWND。
2. **MS Learn**（`https://learn.microsoft.com/en-us/windows/win32/api/exdisp/nf-exdisp-ishellwindows-findwindowsw`）：
   - `pvarLoc`：**必须是 `VT_VARIANT | VT_BYREF`，其值是一个绝对 PIDL**；
   - `pvarLocRoot`：`NULL` 或 `VT_EMPTY`；
   - `phwnd`：**`[out] long *phwnd`** —— 匹配窗口的 HWND；
   - 返回 `S_OK` / `S_FALSE`（没找到）/ `E_NOINTERFACE`（找到但拿不到 IDispatch，仅 `SWFO_NEEDDISPATCH`）/ `E_PENDING`（仅 `SWFO_INCLUDEPENDING`）；
   - 备注：若带 `SWFO_COOKIEPASSED`，`pvarLoc` 被解释为 **cookie 而不是 PIDL**。
3. **ReactOS 实现参考**（`https://cdn.jsdelivr.net/gh/reactos/reactos@master/dll/win32/shdocvw/winlist.cpp`，`WinList_FindFolderWindow`，对应导出序号 178）：

```c
VARIANTARG varg;
InitVariantFromIDList(&varg, pidl);            // VT_ARRAY | VT_UI1 的 SafeArray，装序列化 PIDL
const INT options = SWFO_INCLUDEPENDING | (ppWebBrowserApp ? SWFO_NEEDDISPATCH : 0);
hr = pShellWindows->FindWindowSW(&varg, &s_vaEmpty /*VT_EMPTY*/, SWC_BROWSER, phwnd, options, &pDispatch);
```

   → 印证：`pvarLoc` 是 PIDL、`swClass = SWC_BROWSER(=1)`、`phwnd` 只做输出；且实际调用方用的是 `SWFO_INCLUDEPENDING | SWFO_NEEDDISPATCH`。
   （该文件里 `WinList_GetShellWindows` 本身是 `FIXME` 桩，所以**只**能作为"调用约定"参考，不能作为行为参考。）

### B.3 那 C# 是怎么把 IE 对象和标签 HWND 对上的？

C# 的实现（`E:\use\ai\deepseek\Rust\ExplorerTabUtility\ExplorerTabUtility\Hooks\ExplorerWatcher.cs`）：

```csharp
// :707-735
private Task<nint> GetTabHandle(InternetExplorer window) {
    ...
    if (window is not Interop.IServiceProvider sp) return 0;      // :716
    sp.QueryService(ref _shellBrowserGuid, ref _shellBrowserGuid, out var shellBrowser);  // :718
    shellBrowser.GetWindow(out var hWnd);                        // :723
    _windowEntryDict.UpdateOptionalKey(window, hWnd);             // :726  ← 以标签 HWND 作为第二键
}
// :741-745  纯字典查表，没有"按 HWND 反查 COM"的动作
private InternetExplorer? GetWindowByTabHandle(nint tabHandle)
    => _windowEntryDict.TryGetValue(tabHandle, out var w) ? w : null;
```

关键点：

- `_shellBrowserGuid = typeof(IShellBrowser).GUID`（`:26`），C# 把 **`guidService` 和 `riid` 都填成 `IShellBrowser` 的 IID**（`4C96BE40`/`SID_STopLevelBrowser` 都没用），且实测可用 —— 这是【refimpl】的实测事实，不是文档语义。Rust 侧建议按 §A.7(2) 先试 `SID_STopLevelBrowser`、失败回退 `IShellBrowser::IID`。
- **`IWebBrowser2` 与标签 HWND 的绑定是"建索引"，不是"查询"**：C# 在 `WindowRegistered` 事件里对每个新 IE 对象调一次 `GetTabHandle`，把 `(IE 对象 ↔ 标签 HWND)` 存进 `DualKeyDictionary`；之后按 HWND 取对象只是字典查找。
- 因此 Rust 的正确做法不是"找一条 HWND→COM 的 API"，而是**维护同一张表**（§A.7(2) 的 `enumerate_tabs`）。

### B.4 HWND ↔ COM 的三条现实路径（按推荐度）

| 路径 | 做法 | 成本 | 结论 |
|---|---|---|---|
| **① 枚举 + `QueryService(IShellBrowser).GetWindow()`** | `Count()` + 逐项 `Item(i)` → `cast::<IWebBrowser2>()` → `cast::<IServiceProvider>()` → `QueryService::<IShellBrowser>` → `GetWindow()`，建 `(HWND → IWebBrowser2)` 表 | 每个标签约 4 次进程内/跨进程轻量调用（`Item` 取的是 explorer 里的对象代理）；10 个标签合计 **亚毫秒~几毫秒** 量级 ⚠（未实测） | **推荐**。可缓存，仅在 `WindowRegistered`/WinEvent 后增量刷新 |
| ② `IWebBrowser2::HWND` | 直接读 `HWND` 属性 | 1 次属性读 | ⚠ **未确认**在标签场景下返回标签 HWND 还是顶层 `CabinetWClass`。C# 舍近求远用 ① 本身就暗示它返回顶层窗口。**不要依赖**，但可作为 P0 探针的一条观测项 |
| ③ `FindWindowSW` + `SWFO_COOKIEPASSED` + cookie | 从 `DShellWindowsEvents::WindowRegistered(lCookie)` 拿 cookie，转 `VT_I4` 传 `pvarLoc`，`swfwoptions = SWFO_COOKIEPASSED\|SWFO_NEEDDISPATCH`，一次拿到 `phwnd` + `IDispatch` | 1 次调用，最省 | ⚠ **未确认**：Win11 标签化 Explorer 是否每个标签都注册 cookie。若成立则是最优路径（免枚举）。建议 P0 探针验证 |
| ④ 从 HWND 直接 `QueryInterface` 出 COM | —— | —— | **不存在**。HWND 没有 COM 身份；`WM_HTML_GETOBJECT`+`ObjectFromLresult` 只对 IE 的 `Internet Explorer_Server` 子窗口有效，对 `CabinetWClass`/`ShellTabWindowClass` 无效（见 `https://mail.python.org/pipermail/python-win32/2008-July/007906.html`，那里的链是 `IEFrame → TabWindowClass → Shell DocObject View → Internet Explorer_Server`） |

补充：`IShellWindows` 集合里不只有资源管理器 —— IE 模式窗口、第三方 shell 窗口也可能在里面。建表时应过滤：

```rust
// 只保留 class == CabinetWClass / ShellTabWindowClass 的父窗口
// 或 GetWindowThreadProcessId -> 进程映像名 == explorer.exe
```

### B.5 对 PLAN.md 的影响（建议直接改文档）

`docs/PLAN.md` 需要在三处修订：

1. §3.2「`IShellWindows::FindWindowSW(new_hwnd, …)` → `IWebBrowser2`」→ 改为「枚举 `ShellWindows` 建 `(标签 HWND → IWebBrowser2)` 表；新标签 HWND 从 WinEvent / `FindWindowEx` 拿到后查表」。
2. §5 验收项 2「`FindWindowSW(隐藏窗口 hwnd)` 能否拿到 `IWebBrowser2`」→ 改为「`WindowRegistered` cookie 是否能按标签粒度命中（路径 ③）；否则用枚举（路径 ①）」。
3. §6 风险表「FindWindowSW 对标签 HWND 不支持 → 退回 …」→ 该风险已确定发生，兜底路径 ① 就是主路径。

---

## C. 资源管理器"魔法命令"

### C.1 已确认的三条（一手证据：C# 参考实现）

行号均指 `E:\use\ai\deepseek\Rust\ExplorerTabUtility\ExplorerTabUtility\Hooks\ExplorerWatcher.cs`（全项目 `0xA***` 常量只出现这三处，已 `Select-String "0xA[0-9A-Fa-f]{3}"` 全仓扫描确认）。

| 语义 | `WM_COMMAND` 值 | wParam | lParam | **目标 HWND** | 消息类型 | 依据 | 置信度 |
|---|---|---|---|---|---|---|---|
| 新建标签（≈ Ctrl+T） | `0xA21B` | `0xA21B` | `0` | **`ShellTabWindowClass` 子窗口**（活动标签） | `PostMessage` | `:180-184` | 【refimpl】高 |
| 切到第 n 个标签（≈ Ctrl+n） | `0xA221` | `0xA221` | **`index + 1`** | **`CabinetWClass` 顶层窗口** | `SendMessage` | `:166-167` | 【refimpl】高 |
| 关闭当前标签（≈ Ctrl+W） | `0xA021` | `0xA021` | `1` | **活动标签 `ShellTabWindowClass`** | `SendMessage` | `:290-291` | 【refimpl】中（函数名 `DetachCurrentTab`，语义可能是"拆离"而非"关闭"⚠） |

> **最容易写错的地方**：三条命令的目标窗口各不相同。`0xA21B` 发给**标签子窗口**而不是窗口，`0xA221` 发给**顶层窗口**，`0xA021` 又回到**标签子窗口**。照抄 C# 即可。

C# 原文（供逐字对齐）：

```csharp
// :166-167
// Send 0xA221 magic command (CTRL + 1...n)
WinApi.SendMessage(windowHandle, WinApi.WM_COMMAND, 0xA221, index + 1);

// :183-184
// Send 0xA21B magic command (CTRL + T)
WinApi.PostMessage(tabHandle, WinApi.WM_COMMAND, 0xA21B, 0);

// :290-291
// Send 0xA021 magic command (CTRL + W)
WinApi.SendMessage(activeTabHandle, WinApi.WM_COMMAND, 0xA021, 1);
```

### C.2 关闭整个窗口 / 复制标签 / 恢复标签 / 带路径新建

| 需求 | 结论 | 依据 |
|---|---|---|
| 关闭整个窗口 | **未确认有魔法命令**。可用 `WM_CLOSE(16)` 或 `IWebBrowser2::Quit()`（C# 走后者，`:429`） | C# 只用 `window.Quit()` |
| 复制标签 / 恢复关闭的标签 | **未确认**（C# 未使用，网上无独立佐证） | — |
| **带路径新建标签** | **有，但不是 `WM_COMMAND`，而是 shell verb `opennewtab`**（详见 §D.1） | 【local】`HKCR\Folder\shell\opennewtab` |
| 关闭标签的另一形态 | `0xA021` 在 C# 里叫 `DetachCurrentTab`，"拆离"与"关闭"在 shell 命令语义上可能不同（拆离→新窗口？）。⚠ 需 P0 探针实测：发 `0xA021` 后观察是"标签消失"还是"变成新窗口" | 【infer】 |

### C.3 `SendMessage` 还是 `PostMessage`

- 新建标签：**必须 `PostMessage`**。理由【infer】：建标签会同步触发 Explorer 内部重入（创建 `ShellTabWindowClass`、注册 ShellWindows、发 WinEvent），若在我们的 WinEvent 回调线程里 `SendMessage` 到 explorer，容易在等对方处理时被对方回调回来 → 死锁/重入。C# 也是 Post。
- 切标签：`SendMessage`（C# 用法），同步等待切换完成，无重入风险（不创建对象）。
- 关标签：`SendMessage`（C# 用法）。

### C.4 关于命令号区间的旁证与检索失败记录

- `0xA21B`/`0xA221`/`0xA021` 都落在 `FCIDM_BROWSERFIRST (0xA000) … FCIDM_BROWSERLAST (0xAF00)` 这个 shlobj.h 的 browser command 段内 —— ⚠ **该区间值来自记忆，未在本机 SDK 头文件核实**，报告仅作提示，不作为证据。
- 独立佐证检索**失败**（如实记录）：
  - web 搜索 `"0xA21B"` / `"0xA221"` / `"0xA021"` 被加密货币地址噪声淹没，无有效命中；
  - `autohotkey.com` 论坛相关帖（`GetActiveExplorerTab`、`QuickerAccess`）返回 **Cloudflare 403**；
  - `grep.app` API 返回 **429**；`r.jina.ai`、`duckduckgo lite` 均 fetch failed；
  - wine 的 `dlls/ieframe/shellwindows.c` **404**（文件不存在）；
  - ReactOS `winlist.cpp` 只覆盖 `FindWindowSW` 调用约定，不含 `WM_COMMAND` 魔法命令。
- 结论：**C 部分除 C# 实现外没有第二份独立证据**，因此 P0 探针必须逐条实测（探针设计见 §E.2）。

---

## D. 更早的拦截点（性能关键）

### D.1 【新发现】`opennewtab` shell verb —— 可能是最优合并路径

本机注册表（【local】，`Get-ChildItem HKCR\Folder\shell`）：

```
HKCR\Folder\shell\
  explore           DelegateExecute={11dbb47c-...}  LaunchExplorerFlags=24   ProgrammaticAccessOnly
  open
  opennewprocess    DelegateExecute={11dbb47c-...}  LaunchExplorerFlags=3    ExplorerHost={ceff45ee-...}
  opennewtab        DelegateExecute={11dbb47c-...}  LaunchExplorerFlags=96   MUIVerb=@windows.storage.dll,-8519
  opennewwindow     DelegateExecute={11dbb47c-...}  LaunchExplorerFlags=1    MUIVerb=@windows.storage.dll,-8517
  pintohome         DelegateExecute={b455f46e-...}  MUIVerb=@shell32.dll,-51601
```

```
HKCR\CLSID\{11dbb47c-a525-400b-9e80-a54615a090c0}
  (default)        = CLSID_ExecuteFolder
  InProcServer32   = C:\WINDOWS\system32\explorerframe.dll
  ThreadingModel   = Apartment
```

DLL 字符串（UTF-16LE，【local】）：

| DLL | 字符串 | 偏移 |
|---|---|---|
| `explorerframe.dll` | `opennewtab` | 1344526 |
| `explorerframe.dll` | `LaunchExplorerFlags` / `LaunchExplorer` | 1347224 |
| `explorerframe.dll` | `ShellTabWindowClass` | 1355196 |
| `explorerframe.dll` | `CabinetWClass` | 1357816 |
| `shell32.dll` | `opennewtab` | 3366032（UTF-16）/ 6732064（ASCII） |

**含义**：`opennewtab` 是 Explorer 自己给"在新标签页中打开"用的 verb，`LaunchExplorerFlags=96` 是它的内部启动标志（`opennewwindow=1`、`opennewprocess=3`、`explore=24`）。可以用一次 `ShellExecuteExW` 触发，让 **explorer 自己**建标签并导航：

```rust
use windows::core::w;
use windows::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW, SEE_MASK_INVOKEIDLIST};

pub fn open_in_new_tab(path: &str) -> Result<()> {
    let verb: Vec<u16> = "opennewtab\0".encode_utf16().collect();
    let file: Vec<u16> = format!("{path}\0").encode_utf16().collect();
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST,          // 让 shell 走 IDList/verb 解析（DelegateExecute verb 需要）
        lpVerb: windows::core::PCWSTR(verb.as_ptr()),
        lpFile: windows::core::PCWSTR(file.as_ptr()),
        nShow: 1,                              // SW_SHOWNORMAL
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut sei) }
}
```

（`SHELLEXECUTEINFOW` 结构：Shell:36144，字段 `cbSize/fMask/hwnd/lpVerb/lpFile/lpParameters/lpDirectory/nShow/hInstApp/lpIDList/lpClass/hkeyClass/dwHotKey/Anonymous/hProcess`；`impl Default` 见 Shell:36168 起（按架构 cfg 有多份）；`SEE_MASK_INVOKEIDLIST = 12u32`（Shell:26947）。注意整个结构体被 `#[cfg(feature="Win32_System_Registry")]` 门控，见 §A.0。）

#### D.1.1 与 PLAN 现有链路对比

| | PLAN 现方案 | `opennewtab` 方案 |
|---|---|---|
| 建标签 | `PostMessage(tab, WM_COMMAND, 0xA21B)` | `ShellExecuteExW(verb="opennewtab")` |
| 拿新标签对象 | 20ms 轮询 WinEvent + 50ms 轮询 ShellWindows 集合 | **不需要** |
| 导航 | `Navigate2(path)` + 可选等 `NavigateComplete2` | explorer 内部一次完成，**少一次导航** |
| COM 依赖 | IShellWindows/IWebBrowser2/IServiceProvider | 无 |
| 失败模式 | 命令号随 build 变化 | verb 名随 build 变化（同样有风险，但 verb 是注册表数据，可运行时探测） |

#### D.1.2 附带收益：P0 探针可以用 verb 存在性做"能力探测"

启动时读 `HKCR\Folder\shell\opennewtab` 是否存在、`LaunchExplorerFlags` 是否仍为 96 —— 比硬编码 `0xA21B` 更早发现 build 变化（`0xA21B` 失效时通常表现为"什么都没发生"，很难查）。

#### D.1.3 ⚠ 未确认：标签会落到哪个窗口

`ShellExecuteExW` 没有"目标窗口"参数。合理推断【infer】：handler 用 **当前前台/最近激活的浏览器窗口**。
风险：新窗口刚创建时它自己就是前台窗口 → 标签可能开在**新窗口**里，正好适得其反。

缓解顺序（P0 必须实测确定，二选一）：

1. 先把源窗口 `hide_layered`（alpha=0）+ `SetForegroundWindow(main)`，再调 verb；或
2. 用 `IExecuteCommand`/`IObjectWithSelection` 直接驱动 `CLSID_ExecuteFolder`（crate 里都有：`IExecuteCommand` Shell:10701，方法 `SetParameters/SetDirectory/SetShowWindow/SetNoShowUI/Execute`；`IObjectWithSelection::SetSelection(IShellItemArray)` Shell:15303；`SHCreateItemFromParsingName` Shell:2783、`SHCreateShellItemArrayFromShellItem` Shell:2909），看是否能通过 `SetParameters` 传 `LaunchExplorerFlags`（⚠ 参数格式未确认，`LaunchExplorerFlags` 是 verb 的注册表值还是命令行参数**未知**）。

**建议**：P0 探针第一项就测这个（成本最低、收益最大）。若"落到哪个窗口"不可控，则退回 §A.7 的 `0xA21B + Navigate2` 主路径，两者代码可以并存（配置项切换）。

### D.2 注册表 / 组策略："打开文件夹默认开新标签"

**结论：本 build 未发现此类开关。**

- 【local】枚举 `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced` 全部值名（约 60 个：`AutoCheckSelect`、`HideFileExt`、`SeparateProcess`、`ShowStatusBar`、`TaskbarAl` …），**没有** `OpenFoldersInNewTab` / `OpenInNewTab` / 任何 tab 相关值。
- 【local】ASCII + UTF-16LE 扫描 `C:\Windows\System32\shell32.dll`、`C:\Windows\explorer.exe`、`C:\Windows\System32\ieframe.dll`：**无** `OpenFoldersInNewTab`、`OpenInNewTab`、`TabbedShell` 字符串（shell32 里 `NewTab*` 的命中全是 Edge 的 `NewTabPageFlight*`）。
- 与 Explorer 行为一致：Win11 里"在同一窗口开新标签"是**动作**（Ctrl+T / 中键 / verb），不是**默认策略**。

> 表述建议：写"本机未发现"，并注明方法（枚举 + 双编码字符串扫描），方便日后 build 变化时复核。

### D.3 未公开的标签管理 COM 接口

**结论：未发现可调用的 `IExplorerTabManager` / `ITabManager`。**

- 【local】`ieframe.dll` 里的 `ExplorerTab`（`LinksExplorerTabTemplate`/`LinksExplorerTabBand`）、`TabManager`（仅 `RestoreTabManager`）、`TabBand`（`TabBandClass`/`IETabBand`）都是 **IE7 遗留** UI，与 Win11 标签无关。
- 【local】`shell32.dll` 中 `CabinetWClass` 与 `ShellTabWindowClass` 字符串相邻（UTF-16 偏移 3358504），旁边是 `SHELLDLL_DefView`、`ExplorerBrowserControl`、`combase.dll`、`TaskbandHWND` —— 只是窗口类注册表，不是接口名。
- 【local】**修正**：`C:\Windows\System32\explorerframe.dll` **存在**（3.27 MB，10.0.26100.9949），含 `ShellTabWindowClass`/`CabinetWClass`/`LaunchExplorerFlags`/`opennewtab`。Win11 的标签实现主战场在 **explorerframe.dll + shell32.dll**，不是只有 shell32/ieframe。若要做进一步逆向，优先 `explorerframe.dll`。
- 未确认项：explorerframe.dll 里是否存在未文档化的 `ITab*` 接口。本轮**未做**导出表/接口名全量扫描（字符串扫描不覆盖 vtable 形态的接口），如需可后续用 `dumpbin /exports` 或遍历 CLSID 注册表继续。

### D.4 OLE 拖拽把"新窗口"合并成标签

**结论：理论上可行，工程上不推荐。**

- 标签拖拽走 OLE 拖放（`IDropTarget`/`IDataObject`），但**私有剪贴板格式未确认**；要合成必须知道格式与数据结构。
- 即使知道，程序化 drop 需要：`OleInitialize`(STA) → 构造 `IDataObject` → 调 `DoDragDrop`（会阻塞在拖放循环里，且目标 = **光标下的窗口**）→ 必须先把光标移到主窗口标签栏 → 期间用户鼠标一动就废。
- 收益：省掉"新建标签 + Navigate2"的一次导航。但 §D.1 的 `opennewtab` 已经零导航，**收益被覆盖**。
- 判定：**放弃**。若将来要做"把一个已存在的窗口整体变成标签"（不是重新导航），才值得回来研究这个格式。

### D.5 `EVENT_OBJECT_CREATE` vs `EVENT_OBJECT_SHOW` 的顺序与间隔

**已知（文档语义）**【docs】：

- `EVENT_OBJECT_CREATE (0x8000)`：对象（窗口）被创建时，即 `CreateWindowEx` 内部；
- `EVENT_OBJECT_SHOW (0x8002)`：对象首次显示时，即 `ShowWindow`/`SetWindowPos(SWP_SHOWWINDOW)`；
- 两者都由 out-of-context hook 在**安装线程**上投递；`idObject == 0 (OBJID_WINDOW)`、`idChild == 0` 表示窗口本身。

**推断**【infer】：`explorer.exe /n,<path>` 打开新窗口时，顺序必然是 `CREATE → (导航开始) → SHOW`；`CREATE` 时窗口还不可见，是**隐藏它的最佳时机**（此时改 `WS_EX_LAYERED` 或直接 `SWP_HIDEWINDOW` 都不会闪，因为窗口还没画）。间隔取决于 Explorer 何时 `ShowWindow`，**推测几十 ms 量级**（C# 之所以在 `EVENT_OBJECT_SHOW` 时才隐藏，是因为它只在 SHOW 上挂 hook，并不代表 CREATE 更晚）。

**实测数据：本机未测**（任务限定只写报告、不跑探针、不打开窗口）。PLAN §3.5 的埋点正好覆盖这一点，建议 P0 探针加一条：

```
记录 (event, QPC) 序列：EVENT_OBJECT_CREATE(CabinetWClass) → EVENT_OBJECT_CREATE(ShellTabWindowClass) → EVENT_OBJECT_SHOW(CabinetWClass) → 首次 GetClassName 成功 → LocationURL 可读
```

### D.6 其它"更早拦截"手段评估

| 手段 | 能否"根本不创建窗口" | 可行性 / 风险 | 结论 |
|---|---|---|---|
| `SetWinEventHook(CREATE/SHOW)` | 否（窗口已创建） | 公开 API、out-of-context、不注入、杀软风险最低；`CREATE` 时窗口尚不可见 → 视觉上等于"没出现" | **首选**（PLAN 已选对） |
| `opennewtab` verb（§D.1） | 否（源窗口仍被创建），但**省掉建标签后的第二次导航** | 只需 `ShellExecuteExW`；目标窗口选择规则未确认 | **P0 优先验证** |
| `WH_CBT` / `HCBT_CREATEWND` 全局钩子 | **是**（可在 `CreateWindowEx` 返回前改 `CREATESTRUCT`，甚至直接返回 FALSE 阻止创建） | 必须把 DLL **注入 explorer.exe**（跨进程、位数一致、UIPI/签名、被杀软重点关照）；explorer 崩溃 = 桌面重启 | 功能上限最高、风险最高。**不建议**第一版做 |
| AppInit_DLLs / IFEO / Detours | 是 | 同上，且影响全局进程 | 不做 |
| 劫持 `HKCR\Folder\shell\open` verb（改成自己的 launcher） | 是（连窗口都不建） | 改全局行为：影响所有程序、双击行为、易被"重置默认应用"覆盖；用户安装/卸载体验差 | 不做（但 `opennewtab` 是它的"温和版"：不改行为，只是主动调用 explorer 的能力） |
| 注册表/组策略默认开标签（§D.2） | 是 | **本 build 不存在该开关** | 不可行 |
| 未公开标签 COM 接口（§D.3） | 可能 | 未发现 | 不可行（当前证据） |
| 模拟拖拽合并（§D.4） | 否 | 私有格式 + 需要真实光标 | 不做 |
| UIA / `SendInput` 模拟 Ctrl+T | 否 | 慢、抢焦点、可能误触用户操作 | 仅作最后兜底 |

---

## E. 未确认清单 + P0 探针设计

### E.1 未确认（报告中已逐条标注，汇总在此）

| # | 未确认项 | 影响 | 建议验证方式 |
|---|---|---|---|
| 1 | `IWebBrowser2::HWND` 在标签场景返回标签 HWND 还是顶层窗口 | 若返回标签 HWND，可省掉 `QueryService` | 探针：对一个双标签窗口的两个 IE 对象分别读 `HWND`，对比 `FindWindowEx` 结果 |
| 2 | `WindowRegistered` cookie 是否按标签粒度（`SWFO_COOKIEPASSED` 路径） | 若成立，可免枚举 | 探针：订阅 `DShellWindowsEvents`（crate 无类型化 sink，需手写 `IDispatch` 或先跳过），对比 `FindWindowSW(cookie).phwnd` 与标签 HWND |
| 3 | `opennewtab` verb 的目标窗口规则 | 决定 §D.1 能否作为主路径 | 探针：双窗口场景下调用，观察标签落到哪个窗口 |
| 4 | `0xA021` 是"关闭标签"还是"拆离成新窗口" | 决定关闭逻辑 | 探针：单窗口双标签时发送，观察结果 |
| 5 | 其它魔法命令（关窗口/复制/恢复/带路径建标签） | 功能扩展 | 无证据，暂不实现 |
| 6 | `EVENT_OBJECT_CREATE` → `SHOW` 的实际间隔 | 决定是否需要 CREATE 就隐藏 | 探针埋点（§D.5） |
| 7 | `explorerframe.dll` 是否有未文档化 tab 接口 | 长期优化空间 | `dumpbin /exports` / CLSID 遍历（本轮未做） |
| 8 | wine/ReactOS 对 `FindWindowSW` 的完整实现 | 已用 MS 文档 + ReactOS 调用约定替代 | — |
| 9 | `FCIDM_BROWSERFIRST/LAST` 区间值 | 仅旁证 | 查本机 SDK `shlobj.h`（本轮未查） |

### E.2 P0 探针最小清单（建议顺序，全部在自建测试窗口上做）

1. `HKCR\Folder\shell\opennewtab` 是否存在、`LaunchExplorerFlags` 是否为 96；双窗口下调用后标签落到哪个窗口。
2. 对双标签窗口：`FindWindowEx(CabinetWClass, "ShellTabWindowClass")` 拿到的是不是"活动标签"；标签 z 序变化规律。
3. 三条魔法命令逐条验证（建标签 / 切标签 / 关标签），记录"发到哪个 HWND、Send 还是 Post、lParam 含义"。
4. `IWebBrowser2::HWND` vs `IShellBrowser::GetWindow()` 的返回值对比（确认 §B.4 路径 ②）。
5. WinEvent 时序：`CREATE(CabinetWClass)` → `CREATE(ShellTabWindowClass)` → `SHOW(CabinetWClass)` → `LocationURL` 可读，记录 QPC 差值。
6. `QueryService(SID_STopLevelBrowser)` 与 `QueryService(IShellBrowser::IID)` 的成功率对比。

---

## 附录 A：证据索引

### A.1 `windows` crate 源码

| 主题 | 位置 |
|---|---|
| `IShellWindows` 定义 / 方法 / `FindWindowSW` | `Shell:20836` / `:20853-20872` / `:20889` |
| `SWC_*` / `SWFO_*` | `Shell:27684-27691` |
| `ShellWindows`(CLSID) | `Shell:36922` |
| `IWebBrowser2` / `Navigate2` | `Shell:23973` / `:23985` |
| `IWebBrowserApp::Quit/PutProperty/HWND/ReadyState` | `Shell:24122 / 24128 / 24146 / 24001` |
| `IWebBrowser::Stop/Document/LocationURL` | `Shell:23858 / 23877 / 23921` |
| `IServiceProvider::QueryService` | `Com:3754 / 3763` |
| `IShellBrowser` / `SID_STopLevelBrowser` | `Shell:16921` / `:27268` |
| `IWebBrowser::Navigate` / `IShellView` | `Shell:23846` / `:20662` |
| `IShellFolderViewDual::Folder` / `Folder2::Self_` / `FolderItem::Path` | `Shell:18053` / `Shell:5911` / `Shell:6007` |
| `DShellWindowsEvents`（无类型化 vtbl） | `Shell:5740-5759` |
| `IConnectionPointContainer::FindConnectionPoint` | `Com:1865 / 1878` |
| `CoCreateInstance` / `CoInitializeEx` / `CLSCTX_ALL` / `COINIT_APARTMENTTHREADED` | `Com:105 / 314 / 4865 / 4892` |
| `VARIANT` / `From<i32>` / `From<&str>` / `Drop` | `windows-core\src\variant.rs:6 / 622 / 319 / 44-46` |
| `imp::VARIANT*` union / `VT_*` | `windows-core\src\imp\bindings.rs:575-648 / 657-669` |
| `Param` 四个 blanket impl | `windows-core\src\param.rs:16 / 28 / 45 / 54` |
| `SetWinEventHook` / `WINEVENTPROC` / `HWINEVENTHOOK` | `Acc:309 / 10217 / 9678` |
| `EVENT_OBJECT_*` / `WINEVENT_*` / `WM_COMMAND` | `WAM:3676-3704 / 5220-5222 / 5259` |
| `FindWindowExW` / `GetClassNameW` / `PostMessageW` / `SendMessageW` | `WAM:924 / 1076 / 2434 / 2709` |
| `SetWindowPos` / `SetLayeredWindowAttributes` / `SetWindowLongPtrW` | `WAM:3050 / 2860 / 3026` |
| `ShellExecuteExW` / `SHELLEXECUTEINFOW` / `SEE_MASK_INVOKEIDLIST` | `Shell:4590 / 36144 / 26947` |
| `IExecuteCommand` / `IObjectWithSelection::SetSelection` / `SHCreateItemFromParsingName` | `Shell:10701 / 15304 / 2783` |
| crate features | `windows-0.58.0\Cargo.toml:443,595,646,709,729-734` |

### A.2 C# 参考实现

| 主题 | 位置（`E:\use\ai\deepseek\Rust\ExplorerTabUtility\ExplorerTabUtility\`） |
|---|---|
| 建标签 / 切标签 / 关标签 魔法命令 | `Hooks\ExplorerWatcher.cs:166-167 / :183-184 / :290-291` |
| 标签 HWND 建索引（`QueryService`→`GetWindow`） | `Hooks\ExplorerWatcher.cs:707-735` |
| 活动标签 = z 序第一 | `Hooks\ExplorerWatcher.cs:736-740` |
| `GetWindowByTabHandle`（纯查表） | `Hooks\ExplorerWatcher.cs:741-745` |
| 非文件系统路径兜底（`Folder2.Self.Path`） | `Hooks\ExplorerWatcher.cs:774-782` |
| 合并策略 / 主窗口选择 | `Hooks\ExplorerWatcher.cs:403-429 / 689-706` |
| 隐藏 / 还原窗口 | `Helpers\Helper.cs:385-405 / 406-423` |
| `WindowRegistered` 主触发 + SHOW 次触发 | `Hooks\ExplorerWatcher.cs:905 / 909` |

### A.3 URL

- `FindWindowSW` 文档：`https://learn.microsoft.com/en-us/windows/win32/api/exdisp/nf-exdisp-ishellwindows-findwindowsw`
- ReactOS `winlist.cpp`（`WinList_FindFolderWindow` 调用约定）：`https://cdn.jsdelivr.net/gh/reactos/reactos@master/dll/win32/shdocvw/winlist.cpp`
- IE 的 `WM_HTML_GETOBJECT` 技巧（只对 IE 生效）：`https://mail.python.org/pipermail/python-win32/2008-July/007906.html`

### A.4 本机实测命令与结果

```
# explorerframe.dll 存在性（纠正任务书/前期笔记的错误结论）
Get-Item C:\Windows\System32\explorerframe.dll  -> 3268608 bytes, 10.0.26100.9949
# 双编码字符串扫描（UTF-16LE 偏移）
explorerframe.dll: opennewtab@1344526, LaunchExplorerFlags@1347224, ShellTabWindowClass@1355196, CabinetWClass@1357816
shell32.dll:       opennewtab@3366032(UTF-16), 6732064(ASCII)
# 注册表枚举
HKCR\Folder\shell -> explore, open, opennewprocess, opennewtab, opennewwindow, pintohome
HKCR\CLSID\{11dbb47c-a525-400b-9e80-a54615a090c0} -> CLSID_ExecuteFolder, explorerframe.dll, Apartment
HKCU\...\Explorer\Advanced -> 无 OpenFoldersInNewTab（约 60 个值全枚举）
```

---

## 附录 B：给 `docs/PLAN.md` 的修订建议（按条对应）

| PLAN 位置 | 现文 | 建议 |
|---|---|---|
| §3.2 定位 | `IShellWindows::FindWindowSW(new_hwnd, …)` | 改为"枚举 `ShellWindows` 建 `(标签 HWND → IWebBrowser2)` 表"，并缓存（§B.4 路径 ①） |
| §3.2 兜底 | `IShellBrowser → IShellView → IPersistFolder2` | 改为 `Document → IShellFolderViewDual::Folder → Folder2::Self_ → FolderItem::Path`（§A.3.1，与 C# 一致） |
| §3.3 并入 | `0xA21B` + 轮询 + `Navigate2` | 保留为方案 B；新增方案 A：`ShellExecuteExW(verb="opennewtab")`（§D.1），P0 先验证目标窗口规则 |
| §5 验收 2 | `FindWindowSW(隐藏窗口 hwnd)` | 改为 cookie 路径（未确认）+ 枚举路径（可行） |
| §6 风险表第 2 行 | `FindWindowSW` 对标签 HWND 不支持 → 兜底 | 该风险已**确定发生**；兜底即主路径 |
| §6 风险表第 1 行 | 魔法命令号随 build 变化 | 增加"启动时探测 `opennewtab` verb 是否存在、`LaunchExplorerFlags` 是否为 96"作为更早的失效信号 |
