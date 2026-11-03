# exptab — 新窗口自动并入标签（Rust 版）

Windows 11 资源管理器（Explorer）的"新开窗口自动并入同一主窗口的标签"工具，
是 C# 版 [ExplorerTabUtility](https://github.com/wangfu91/ExplorerTabUtility) 核心功能的
单文件 Rust 重写版。只依赖 `windows` crate，release 单文件约 **380 KB**。

**v0.1 范围**：只做核心功能 —— 新窗口自动合并进主窗口标签 + 运行时开关。
没有托盘界面、没有热键设置界面、没有"恢复已关闭标签"等附加功能。

> **下载**：[Releases](https://github.com/rosefinchlj/exptab-rs/releases) 提供最新单文件 exe（Windows 11 x64，免安装）。
> 本项目同时作为 **Rust 实践学习项目** 开源：Win32 窗口/COM 自动化、事件驱动检测、单文件 daemon + 回环 TCP 控制通道。

---

## 1. 构建

```powershell
cd exptab-rs
cargo build --release
# 产物: target\release\exptab.exe
```

- 依赖：Rust 1.99+（`stable-x86_64-pc-windows-msvc`）、仅 `windows = "0.58"`（静态链接，无需 VC 运行库以外的组件）。
- release profile 已开 `opt-level=3 + lto + codegen-units=1 + panic=abort`。
- 开发用 `cargo build`（debug）同样是完整功能，只是稍慢。

## 2. 使用

```powershell
.\target\release\exptab.exe            # 确保后台 daemon 在跑（双击可用）
.\target\release\exptab.exe run        # 前台运行 daemon（Ctrl+C 退出）
.\target\release\exptab.exe start      # 后台启动（不占控制台窗口）
.\target\release\exptab.exe status     # 状态：开关、运行时长、主窗口、成功/失败/放行计数
.\target\release\exptab.exe off        # 关闭合并（新窗口保持独立）
.\target\release\exptab.exe on         # 重新打开
.\target\release\exptab.exe toggle     # 切换
.\target\release\exptab.exe metrics    # 最近 8 条合并指标（CSV 行）
.\target\release\exptab.exe rescue     # 救回被隐藏/挪到屏幕外的资源管理器窗口
.\target\release\exptab.exe autostart on|off   # 开机自启 / 取消自启
.\target\release\exptab.exe quit       # 停止后台 daemon
```

单实例：第二个 daemon 会直接退出并提示（回环 TCP 端口占用检测）。
客户端命令也走同一端口（端口号记在 `%LOCALAPPDATA%\exptab\ipc.port`）。

### 想让某个新窗口"独立存在"

- 打开窗口时按住 **Ctrl+Shift**（默认开启，配置项 `bypass_ctrl_shift`）。
- 或先 `exptab off`，开完窗口再 `exptab on`。
- 带多个标签的新窗口不会被合并（配置项 `merge_single_tab_only`）。

## 3. 配置

`%LOCALAPPDATA%\exptab\config.ini`（daemon 停止时编辑；`on/off` 可直接在运行时切换：

```ini
enabled=true                 ; 总开关
merge_single_tab_only=true   ; 只有"单标签新窗口"才合并
bypass_ctrl_shift=true       ; 按住 Ctrl+Shift 时放行
detect_poll_ms=20            ; 兜底轮询上限（1-50ms）；正常靠事件唤醒，见 §15
detect_poll_idle_ms=100      ; 没有任何资源管理器窗口时的兜底轮询（1-1000ms）
max_wait_ms=3000             ; 等待新窗口在 IShellWindows 注册的上限
metrics=true                 ; 每次合并写一行 CSV
main_hwnd=0x0                ; 0=自动选主窗口；也可写死某个窗口句柄
```

合并指标：`%LOCALAPPDATA%\exptab\metrics.csv`（每次合并一行，含各阶段耗时；
文件可随时删除，daemon 下次写入会重建表头）。

## 4. 实现原理（为什么它比 C# 版更早介入）

1. **检测**：检测线程平时**阻塞在内核里**（`WaitForSingleObject`），由两个来源唤醒：
   ① 进程内的 `SetWinEventHook`（`EVENT_OBJECT_CREATE..EVENT_OBJECT_SHOW` 与
   `EVENT_SYSTEM_FOREGROUND`，OUTOFCONTEXT 回调，绝不注入 Explorer 线程）；
   ② `detect_poll_ms`（默认 20ms，仅作漏事件的兜底）。
   醒来后用 `FindWindowExW(NULL, after, "CabinetWClass", NULL)` 做**增量**扫描
   （一次调用的常见情况就是"没有新窗口"，~14 µs），新句柄即新窗口。
   事件只是"提示"，正确性始终由扫描保证，所以事件迟到或漏报都不会漏合并。
   （早期版本靠 2ms 忙轮询，那正是空转 CPU 的来源，见 §15。）
2. **立刻隐藏**：新窗口一出现就 `ShowWindowAsync(SW_HIDE)`（非阻塞，18 µs），
   再加一个 1ms 轮询的 watchdog 盯着 `IsWindowVisible`，一旦 Explorer 把它显示出来
   就再按下去；随后用 `WS_EX_LAYERED + alpha=0` + 移出屏幕做"硬隐藏"
   （保留 `WS_VISIBLE`，避免打断 Explorer 初始化）。
   → 由于检测发生在 Explorer 真正 `ShowWindow` 之前约 200ms，通常用户全程看不到窗口。
3. **先建目标标签，再等注册**：向主窗口的第一个标签 `PostMessage(WM_COMMAND, 0xA21B, 0)`
   （= Ctrl+T，实测 3–6µs）**立刻**新建目标标签——此时还不知道源路径。
   标签子窗口在 **detect 后 ~20ms** 就出现，用户看到标签的时间因此大幅提前。
4. **并行等等**：在同一个循环里等两件事，`IShellWindows::Count()` 一变就做一次快照
   （`Item() → IWebBrowser2`，再用 `IServiceProvider → IShellBrowser::GetWindow`
   把 IE 对象映射回标签 HWND）：① 源标签注册（读 `LocationURL` 得到路径）；
   ② 目标标签注册。两者谁先到都记录，都到齐就对目标标签的
   `IWebBrowser2` 调 `Navigate2(url)`（跨进程必须传 4 个非空 VARIANT，否则
   `RPC_X_NULL_REF_POINTER 0x800706F4`）。实测目标标签的注册只比源标签晚 20–45ms，
   于是"建标签"和"等源路径"两段等待被重叠掉了。
5. **关源窗口**：`WM_COMMAND 0xA021 lParam=0` → 400ms → `IWebBrowser2::Quit()`
   → 1.5s → `WM_CLOSE` → 800ms 三级兜底（Explorer 在忙时单种方式可能失败）。
6. **安全网**：Ctrl+C / 关控制台 / `quit` 时把本工具隐藏掉的窗口再 `unhide`；
   daemon 启动时也会扫描并恢复上次崩溃遗留的隐藏窗口。

## 5. 同机实测（Rust release vs C# ExplorerTabUtility 2.5.0）

同一台机器、同一主窗口、各 10 次打开 `explorer.exe /n,<新目录>`，外部脚本
（`tools\bench-merge.ps1`）测量"进程启动 → 标签出现在主窗口"：

| 指标 | C# 版 | Rust 版（提前建标签） | 说明 |
|---|---|---|---|
| 新窗口被检测到 | 307 ms (p50) | **323 ms** (p50, p95 461) | Rust 早于窗口显示 |
| 用户看到标签 | 574 ms (p50, p95 625) | **342 ms** (p50, p95 493) | **快约 40%**，尾部更稳 |
| 标签内容开始导航 | ~1.0 s（估） | **detect+~600 ms（≈spawn+0.93 s）** | 受 Explorer 标签浏览器初始化限制 |
| 源窗口闪现 | 0/10 | p50 0 ms，p95 29 ms，最差 44 ms | 见 §4 |
| 源窗口真正关闭 | 1042 ms (p50) | 1506 ms (p50) | 此时已不可见，用户无感 |
| 常驻内存（私有集） | 105.6 MB | **10.7 MB** | 工作集 134 MB vs 40.5 MB |
| 空闲 CPU（单核） | 1.25% | **0.3–0.8%**（见 §15） | 事件驱动等待；旧版 2ms 轮询为 2–5% |
| 分发形态 | 多文件 + .NET Framework 4.8.1 | **413 KB 单文件 exe** | 无运行时依赖 |

结论（详见 `docs/REPORT.md`）：**标签出现时间比 C# 版快约 40%**（p50 0.34s vs 0.57s，
p95 493ms vs 625ms）。关键改动是第 4 节的"先建目标标签、再并行等两边的注册"：
把目标标签的 ~500ms Explorer 注册等待与源窗口的注册等待重叠起来。
内容就绪时间受 Explorer 自身"新建标签浏览器初始化"限制，两边都在 1s 量级。
Rust 的额外收益是单文件大小与内存（1/10 私有集）。

## 6. 已知局限

- 只在 Windows 11（Explorer 标签模型）验证过：本机 Windows 11 26H2 / build 26300，
  Explorer 10.0.26100。Windows 10 无标签，不适用。
- 没有托盘图标/开机自启（v0.1 有意省略）。
- 多标签新窗口、Ctrl+Shift 放行逻辑已实现，但自动化测试难构造，属"代码已就绪、
  实测有限"。
- 无代码签名，首次运行可能触发 SmartScreen。
- 偶尔的 ≤44ms 闪现出现在 Explorer 冷启动特别慢时（隐藏操作要等 Explorer 消息泵）。
- 依赖 `0xA21B / 0xA021 / 0xA221` 三个未公开的 Explorer 命令号（与 C# 版同源），
  未来 Windows 版本可能失效；失效时表现为 `new_tab_timeout` 等指标，窗口会被放行显示。

## 6.1 出问题时怎么救

- 如果某个资源管理器窗口"不见了"（被隐藏后没恢复、或崩溃遗留），运行
  `exptab rescue`：把处于 `alpha=0` 或停在屏幕外 (-32000) 的窗口全部还原到屏幕内。
  daemon 每次启动时也会自动做一次。
- 升级到本版后，隐藏时会记住窗口原来的位置，放行/失败时会原样还原，不会再出现
  "窗口被挪到屏幕外看不见"的情况。
- daemon 启动时如果系统里还没有任何资源管理器窗口，**第一个新窗口会被当作主窗口**
  放行显示（而不是被当成待合并窗口隐藏），之后的新窗口才会并入它。


## 7. 目录

```
src/win.rs      Win32 封装（窗口扫描、隐藏/恢复、Explorer 命令号、按键状态）
src/com.rs      IShellWindows / IWebBrowser2 / IShellBrowser 封装
src/engine.rs   检测线程 + worker + 合并线程（核心逻辑）
src/cfg.rs      config.ini 读写
src/ipc.rs      单实例 + 回环 TCP 控制通道（status/on/off/toggle/quit/metrics）
src/metrics.rs  合并指标 CSV
src/probe.rs    开发用 P0 探针（会开测试窗口，普通用户不需要）
tools/          bench-merge.ps1（本工具与 C# 版同机对比）、bench.ps1（WinEvent 时间线）、p0.ps1
docs/PLAN.md    方案、docs/REPORT.md 实测报告、docs/research-api.md API 考证
```

## 8. Win+E（主文件夹）等虚拟文件夹窗口

Win+E 打开的是 **Home 虚拟文件夹**（`::{F874310E-B6B7-47DC-BC84-B9E6B38F5903}`），它的
`IWebBrowser2::LocationURL` 是空字符串，`Document.Folder.Self.Path` 才是该 CLSID 路径。
v8 起做了两处处理：

1. 取位置时先 `LocationURL`，为空则走 `Document → IShellFolderViewDual::Folder →
   Folder2::Self → FolderItem::Path` 兜底（与 C# 版 `GetLocation` 等价）；
2. 导航时若路径是 `::{}` 命名空间（或含 `#` / `%23`），不再传字符串 URL，而是
   `Shell.Application.NameSpace(path)` 得到 `Folder` 自动化对象，用 **VT_DISPATCH**
   形式的 VARIANT 调 `Navigate2`（传字符串会被 Explorer 以 `E_INVALIDARG` 拒绝）。

实测（Release，pin 一个 scratch 主窗口）：连续 4 次 `Win+E` 等价操作全部合并为
`主文件夹` 标签，源窗口全部关闭，`merges ok=4 failed=0`，标签在 15 秒后仍在原窗口。

另外 v8 增加了真正的单实例保护（命名事件 `Local\exptab-rs-daemon`），不会再出现两个
daemon 同时合并同一窗口的情况。

> 已知风险：Explorer 的 `WM_COMMAND 0xA021` 系列“关标签/关窗口”魔法命令在个别状态下
> 会关掉整个窗口，本工具只在**自己刚放行的窗口**上使用它们（`lParam=0` 关源窗口、
> `lParam=1` 回滚多余标签），不会对用户窗口做试探性发送。如果仍有窗口异常消失，
> 用 `exptab rescue` 恢复被隐藏窗口，或查看 `exptab metrics` 里的 result 列。

## 9. v10 体验改进：零闪屏 + 重复目录去重 + 开机自启

用户实测反馈"第一次打开能看到窗口闪一下"。根因与修复：

- v7 只在检测到窗口时发一次 `ShowWindowAsync(SW_HIDE)`，真正的"移出屏幕 + 全透明"推迟到
  源窗口注册之后（约 250-500ms）。Explorer 初始化结束时又会 `ShowWindow`，于是窗口在
  "被重新显示"和"被彻底藏起来"之间存在一段**真正在屏幕上的可见时间**（冷启动第一次最长）。
- v10 改为**检测瞬间**调用 `win::hide_async()`：`SetWindowPos(..., -32000, -32000,
  SWP_ASYNCWINDOWPOS)` + `ShowWindowAsync(SW_HIDE)`，约 20µs 返回。`SWP_ASYNCWINDOWPOS`
  把移动请求投递给窗口线程而**不等待**（同步版本在 Explorer 初始化期会阻塞 90-220ms，
  这也是之前不敢在检测时隐藏的原因），所以窗口在 Explorer 显示它之前就已经在屏幕外了。
- 用"位置感知探针"（每 1ms 检查 `IsWindowVisible && GetWindowRect().Left > -30000`）
  连续 10 次实测：**窗口在屏幕上的可见时间 = 0ms**（`onscreen_at = -1`）。

另外两个"比 C# 更好用"的点：

1. **重复目录不重复开标签**：新窗口要打开的目录如果已经存在于主窗口的标签里，
   不再建重复标签——直接关掉新窗口、保留你原来的标签（`exptab metrics` 里 result=`deduped`）。
   C# 版默认会产生重复标签。
2. **开机自启一条命令**：`exptab autostart on` 写入 `HKCU\...\Run`（当前用户，无需管理员），
   `off` 删除；`exptab` 不带参数即"确保后台在跑"，双击 exe 就能用。

实测（10 次，release，pin scratch 主窗口）：新标签出现 p50 **408ms**（C# 版 574ms），
`detect→hide` p50 185ms，真实屏幕闪屏 0/10。
## 10. v11：第一次打开不闪、Win+E 合并后主窗口回到前台

用户反馈两件事，都是真实缺陷：

1. **第一次打开窗口会"闪一下，然后才显示出来"**。原因：daemon 刚启动时系统里还没有主窗口，
   这个新窗口会被先当成待合并源隐藏（因为要等标签建好才能判断），发现没有可并目标后才
   提升为主窗口并还原——于是用户看到"闪一下 + 延迟约 1 秒才出现"。
   修复：检测到窗口时如果**主窗口还不存在**，先不隐藏（`adopt_candidate`），等确认它是
   第一个窗口就直接采纳为主窗口，全程不隐藏，打开即正常显示。
2. **Win+E 打开后桌面看不到窗口，但任务栏显示已打开**。原因：Win+E 的窗口被合并/去重到
   主窗口后，主窗口没有被提到前台（可能被其它窗口挡住或最小化），用户以为"没打开"。
   修复：合并/去重成功后调用 `win::activate_window(main)`：最小化则 `SW_RESTORE`，
   然后 `SwitchToThisWindow` + `SetForegroundWindow`（`SwitchToThisWindow` 能绕过前台锁，
   等价于 Alt+Tab 切过去），并保持新标签处于选中态。

实测（Release）：Win+E 连续两次 → 都 `deduped`（主窗口已有"主文件夹"标签），完成后
前台窗口 = 主窗口；普通新窗口合并 → 前台窗口 = 主窗口且新标签是活动标签。
## 11. v12：Win+E 反复打开后窗口"消失"彻底修复

用户反馈（真实现象）：连按多次 Win+E，任务栏有图标，桌面上却一个资源管理器窗口都看不到。

**根因（两层）**

1. Explorer 会把备用的池化 `主文件夹` 窗口**停在 (-32000,-32000)**，Win+E 时直接复用。
   本工具检测到它时，把这个"屏幕外坐标"当成了窗口的原始位置记录下来；
   放行（`unhide`）时又"原样还原"到 -32000 —— 于是窗口可见、在任务栏、但屏幕外。
2. `exptab rescue` / 启动自愈只认"layered + alpha=0"的隐藏窗口，这种"只有坐标不对、
   没有透明属性"的窗口救不回来。

**修复（`src/win.rs` / `src/engine.rs`）**

- `sane_rect()`：记录原始位置时做健康检查（左/上 ≤ -30000、宽高 < 200 视为无效），
  无效则用默认位置 (120,90,1400,890)，从根上不把屏幕外坐标当"原始位置"。
- `unhide()` 末尾再读一次 rect，仍异常则强制放到默认位置。
- `is_offscreen()` + **1 秒自愈巡检**：任何 CabinetWClass 只要屏幕外、且不属于正在合并的任务，
  1 秒内被自动还原（包括崩溃/重启遗留的窗口）。
- 主窗口选择（`pick_main` / `ensure_main` / `other_tabbed_window`）排除屏幕外窗口。
- `activate_window()` 前先检查并还原屏幕外窗口。
- 去重/合并成功时**先激活主窗口、再关源窗口**，用户不用等源窗口关闭就能看到结果；
  去重时还会自动选中主窗口里已存在的那个 `主文件夹` 标签。
- 新安全规则：`close_own_tab()` 只在标签数 ≥ 2 时关闭，且只关自己刚建的标签
  （对最后一个标签发 `0xA021 lParam=1` 等于 Ctrl+W 关窗，会关掉整个窗口）。

**实测**

- 连续 8 次 Win+E（3.5s 间隔）：全部被处理，结束无屏幕外窗口，前台 = 主窗口。
- 干净状态（没有任何资源管理器窗口时启动 daemon）：第一次 Win+E 直接可见并采纳为主窗口
  （不隐藏），之后 4 次全部去重到该窗口，前台始终是主窗口。
- 普通目录合并 3 次 + 位置感知探针：`onscreen_at = -1`，屏幕可见时间 0ms。

## 12. v13：识别并跳过新版（WinUI3/XAML）资源管理器窗口

用户截图现象：一个 `主文件夹` 窗口，标签栏两个标签（第二个空白），内容区全白，
点 × / Ctrl+W / 关标签都没反应——**无法关闭**。

**这是什么窗口**

Windows 11 26H2（build 10.0.26300 实测）上新版 WinUI3/XAML 文件资源管理器窗口：
外壳仍然是 `CabinetWClass`，但标签栏整块是 XAML（`Microsoft.UI.Xaml.Controls.TabView`），
**没有任何 `ShellTabWindowClass` 子窗口**。于是：

- 经典标签消息（`0xA21B` / `0xA021` / `0xA221`）对它完全不适用；
- 旧版工具把它当普通窗口隐藏/移动后，XAML 内容会渲染成白板，并且拒绝关闭。

**识别方法**

- 经典窗口（build 26300）的标签宿主 `ShellTabWindowClass` 在**窗口创建后 15–20ms** 就出现，
  而 WinUI3 标题栏子窗口（`TITLE_BAR_SCAFFOLDING_WINDOW_CLASS` /
  `Microsoft.UI.Content.DesktopChildSiteBridge`）要**约 450ms 后**才出现；
- 新版窗口则相反：一起步就是 XAML，且永远没有 `ShellTabWindowClass`。
- 所以：检测瞬间就有 XAML 子窗口 → 新版窗口，直接跳过；
  合并等待标签期间（60ms 后）出现"有 XAML、无经典标签宿主" → 同样判定为新版窗口，立即放行。

`win::is_new_style_explorer()` = 有 XAML 子窗口 **且** 没有 `ShellTabWindowClass`，
用于检测、自愈巡检与 `rescue`：这类窗口**绝不隐藏、不移动、不合并、不救援**。

**顺带修复：Explorer 自己的池化备用窗口**

Explorer 会把备用 `主文件夹` 窗口停在 (-32000,-32000)，但 `IsWindowVisible = True`。
v12 的 1 秒自愈会把它们搬回屏幕内，破坏 Explorer 的 Win+E 池（复用后位置错乱）。
现在只救"确实是我们藏的"窗口：`hidden_by_us()`（layered + alpha 0）或
"屏幕外 + 不可见"；Explorer 的可见池化窗口原样不动。

**实测（v13 release）**

- 4 次普通目录打开：位置感知探针 `onscreen_at` 为空（窗口从未出现在屏幕上，0ms 闪屏），
  全部合并进主窗口；
- 4 次 Win+E：全部合并/去重，主窗口始终可见且在前台，结束无屏幕外残留、
  无新版窗口被碰（`new_style = 0`）。
- 那个卡死的窗口最终只能重启资源管理器清掉（`taskkill /F explorer.exe` 后重新启动，
  桌面/任务栏自动/手动恢复）；新版窗口在卡死状态下 `WM_CLOSE`、`SC_CLOSE`、
  UIA `WindowPattern.Close()`、UIA 关标签、Ctrl+W 全部无效。

## 13. v14：Win+E 改成"此电脑"后的窗口不可见（异步移屏竞态）

用户把 Win+E 的设置从"主文件夹"改成"此电脑"
（`HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced\LaunchTo = 1`）后，
连续 Win+E 又出现"任务栏上看已经打开了，但桌面上看不到窗口"。

**根因：`unhide()` 与还没被处理的异步移屏打架**

- `hide_async()` 用 `SetWindowPos(..., -32000, -32000, SWP_ASYNCWINDOWPOS)` 把窗口移出屏幕，
  这是**投递**给 Explorer 窗口线程的请求，不等它执行完就返回；
- 走 `no_main_window` 分支时，merger 线程立刻用 `unhide()` 同步还原窗口；
- 但 Explorer 窗口线程随后才处理那条已排队的 park 消息，把窗口又挪回 (-32000,-32000)；
  此时 `IsWindowVisible = True`、没有 layered 标志 → 窗口"存在但看不见"。
- 触发条件："关掉主窗口后立刻（≈250ms 内）再按 Win+E"。此时没有主窗口可合并，
  走 `no_main_window` → 隐藏与还原几乎同时发生 → 竞态。

**修复（`src/win.rs`）**

- `place(hwnd, rect)`：`SetWindowPos(..., SWP_NOZORDER|SWP_NOACTIVATE|SWP_SHOWWINDOW)` +
  `ShowWindow(SW_SHOWNA)`。
- `unhide()` 重写：先 `place()` 到 `sane_rect()` 的目标位置，再**投递一次同位置的异步
  `SetWindowPos`**（排在待处理的 park 之后执行，覆盖它），最后调用
  `settle_onscreen(hwnd, target)`：150ms 内每 5ms 检查一次，只要窗口不在屏幕内或不可见就
  再 `place()` —— 迟到的 park 无论何时到达都会被纠正回来。

**实测（release）**

- 3 轮"关主窗口 + 250ms 后打开"：每轮结束都是 1 个可见窗口、`off=False`、位置 (22,404)，
  不再出现屏幕外窗口；`no_main_window` 对应的窗口也都能正常显示。
- 两种 Win+E 模式兼容（同一 daemon、同一主窗口）：
  - Home 命名空间（`explorer.exe "/n,::{F874310E-B6B7-47DC-BC84-B9E6B38F5903}"`）→ `merged`，
    在主窗口里新增"主文件夹"标签；
  - 此电脑（`Start-Process explorer.exe`，等价于 LaunchTo=1 的 Win+E）→ 已有同名标签时
    `deduped`，不重复开标签；
  - 再连开 3 次此电脑：全部 `deduped`，主窗口始终可见，`merges ok=7 failed=0`。
- 两种设置都不需要额外配置，程序按窗口的实际 URL/命名空间判断，去重也按内容判断。

## 14. v15：重复打开同一窗口时不再"先建标签再关掉"

主窗口已经有"此电脑"（或某个文件夹）标签时，再按 Win+E，之前会先看到一个一模一样的
新标签出现约 0.7–0.9 秒、然后消失。现在重复打开会**直接选中已有的那个标签**，
全程不创建新标签。

**根因**：为了和源窗口注册并行，合并流程会先 `0xA21B` 建目标标签；等两边都注册完
（约 1s）才发现 URL 和已有标签重复，再 `close_own_tab()` 把刚建的标签关掉——
于是"重复标签"短暂可见。

**修复（`src/engine.rs`、`src/win.rs`）**

- `wait_source_title()`：新窗口标题先是通用的"文件资源管理器"，约 80–100ms 后才变成
  `<folder> - 文件资源管理器`；短暂轮询（最多 400ms）拿到有信息量的标题。
- `duplicate_hint()`：源窗口标题是 `<name> - 文件资源管理器`，主窗口里已有标题为
  `<name>` 的标签 → 视为"可能重复"的候选（只匹配标题，最终以 URL 为准）。
- 命中候选后先用 `wait_source_entry()` 等源窗口注册（约 0.6–1.0s，**期间不建标签**）：
  - URL 与候选标签一致 → `select_tab_handle()` 选中已有标签 → `activate_window()`
    把主窗口带回前台 → `close_source()` 关掉隐藏的源窗口，返回 `deduped`；
  - URL 不一致（同名不同目录）→ 退回正常合并流程，不会误关任何东西。
- `select_tab_handle()`：0xA221 只接受"视觉序号"，而子窗口枚举顺序是 Z-order
  （活动标签在最前，约等于视觉顺序的反向），所以先按 `count - 1 - pos` 猜一次并验证
  活动标签是否切换，失败再逐个索引枚举（每次 15ms、最多 8 次复查）。

**实测（release，daemon 运行，主窗口 4–5 个标签）**

| 场景 | 结果 |
| --- | --- |
| 反复 Win+E（此电脑）×3 | 标签数始终 4，采样期间 0 次变化；`new_tab_us = 0`，`perceived` 0.57–1.01s |
| 反复打开同一文件夹 ×3 | 标签数始终 4，0 次变化 |
| 反复打开"主文件夹"命名空间 ×2 | 标签数始终 4，0 次变化 |
| 打开新文件夹（对照） | 正常 `merged`，4→5 个标签，`new_tab_us = 0.50s`，`perceived 1.64s` |

去重时主窗口会被切到已有的那个标签（实测标题从"主文件夹 和 3 个其他选项卡"变成
"此电脑 和 3 个其他选项卡"），然后回到前台。

## 15. v16：空转 CPU 从 2–5% 降到 ≤0.6%

**问题**：之前"开着不用"时 daemon 会持续占用 1–5% 的一个核心（按机器上其他程序
有没有调大系统定时器精度而浮动）。实测（`tools\idle-cpu.ps1`，按线程采
`TotalProcessorTime`）：

| 状态 | 旧版 | 现在 |
| --- | --- | --- |
| 没有资源管理器窗口 | 1.87% / 1.46% / 1.14%（三轮 15s） | **0.31%** |
| 有一个资源管理器窗口 | 2.18%–4.99%（用户实际运行 5 小时的进程） | **0.26%–0.78%**（多轮） |

**根因（两个叠加）**

1. **2ms 忙轮询**：检测线程每 2ms 醒来扫一次窗口 —— 每秒 ~500 次唤醒。
2. **`thread::sleep` 的对齐**：系统定时器最小分辨率是 15.6ms，`sleep(2ms)` 实际睡到
   下一个 tick（~31ms 醒一次也可能是 15.6ms）。所以"2ms 轮询"要么被拖慢，要么在
   别的程序调过 `timeBeginPeriod` 时才精确 —— 这正是空闲 CPU 在 0.3%–5% 之间乱跳的原因
   （用 `tools\wake-wakes.ps1` 实测：请求 10ms 超时的等待，实际每秒只醒 64.6 次，全部
   是超时唤醒、事件唤醒 0 次）。

**修复**

- `win::wait_detection_event(tick_ms)`：线程阻塞在 `WaitForSingleObject(event, tick)`
  上，**不再自己轮询前台窗口**。手动重置事件由 `SetWinEventHook` 回调触发：一个钩子
  同时覆盖 `EVENT_SYSTEM_FOREGROUND`（3）到 `EVENT_OBJECT_SHOW`（32770），所以
  "Explorer 建窗/显示窗口"和"用户切换前台窗口"都会立刻把它叫醒；`tick` 只是兜底。
- `Heartbeat`（`CreateWaitableTimerW`）用于需要精确短睡的地方，不受 15.6ms 限制。
- `detect_poll_ms` 默认从 **2 改成 20**（现在它只是"漏了事件"的安全网，不是主路径）；
  没有资源管理器窗口时用 `detect_poll_idle_ms`（默认 100）。
- 增量扫描 `win::new_cabinet(&seen)`：常态下一次 `FindWindowExW` 就返回"没有新窗口"，
  不再每秒全量枚举。

**验证（release，`detect_poll_ms=20` / `detect_poll_idle_ms=100`）**

| 项目 | 结果 |
| --- | --- |
| 空闲 CPU，无窗口 | **0.31%**（原 1.87%） |
| 空闲 CPU，有窗口 | **0.26%–0.78%**（原 1.04%，更早 2.7%） |
| 反复 Win+E（此电脑）×2 | 全部 `deduped`，标签数不变，主窗口保持可见 |
| 新文件夹合并 | `merged`，标签 1→2、2→3，主窗口保持可见 |
| 闪屏（位置感知探针 1ms 采样） | `onscreen_at = -1`，**窗口从未出现在屏幕上（0ms）** |
| 一次 Win+E 期间的总 CPU | 0.016 s |

**新增的自愈**：每秒的巡检除了修复"被我们停到屏幕外"的窗口，现在也修复"位置正常、
`IsWindowVisible` 却是 false"的**主窗口**（用户在任务栏能看到、桌面上却什么都没有的
那种状态）。只碰主窗口、且跳过 new-style WinUI3 窗口与正在合并中的源窗口。