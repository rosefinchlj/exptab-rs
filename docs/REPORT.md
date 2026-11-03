# exptab 实测报告（Rust 版 vs C# ExplorerTabUtility 2.5.0）

日期：2026-11-02（v1–v5）/ 2026-11-03（v6 提速）/ 同机对照
机器：Windows 11（build 26300，DisplayVersion 26H2），Explorer 10.0.26100.1710
Rust 版：`exptab-rs` release 构建（`target\release\exptab.exe`，389,632 B，LTO）
C# 版：ExplorerTabUtility 2.5.0（.NET Framework 4.8.1 WPF）

---

## 1. 测量方法（外部、工具无关）

`tools\bench-merge.ps1` 不依赖被测工具的任何埋点，只观察窗口状态：

1. 记录主窗口当前标签集合；
2. `Start-Process explorer.exe /n,<新目录>` 并记录 `spawn` 时刻（QPC）；
3. 2ms 轮询：
   - **detect**：新 `CabinetWClass` 顶层窗口出现；
   - **hide**：该窗口 `WS_EX_LAYERED` + `alpha==0`（C# 与本工具都用这个隐藏手法）；
   - **tab**：主窗口出现新的 `ShellTabWindowClass`，且标题与新目录的叶子名一致；
   - **close**：源窗口 `IsWindow` 变 false；
4. `flashed = hide - visible_start`：窗口从"可见"到"被隐藏"的时长；从未隐藏记录为哨兵值 9999。

热循环只用廉价 Win32 调用（~2µs/次）；`GetWindowTextW` 是跨进程阻塞调用，故标题
只取"标签出现后"的确认时刻，不进热循环（否则会显著拖慢 Explorer、污染数据）。

规模：每个配置 10 次（个别早期配置 2–5 次），每次新目录，主窗口固定为同一个
scratch 窗口（`main_hwnd` pin），避免与用户窗口互相干扰。两种工具从不并发运行。

## 2. 总表（spawn → 各时刻，单位 ms，p50 / p95）

| 配置 | 说明 | detect p50 | hide p50 | **tab p50** | tab p95 | close p50 | flash |
|---|---|---|---|---|---|---|---|
| C# ExplorerTabUtility | 基线 | 307 | 526 | **574** | 625 | 1042 | 0/10 |
| Rust v1 | detect 时同步隐藏 | 321 | 429 | **976** | 1011 | 1845 | 0/10 |
| Rust nohide | `EXPTAB_NOHIDE=1`，不隐藏 | 322 | – | **573** | 621 | 1740 | 9999（全程可见） |
| Rust v2 | 异步 `SW_HIDE` + 注册后 layered 隐藏 | 311 | 544 | **544** | 917 | 1654 | 3/10（≤422ms） |
| Rust v3 | v2 + 可见性 watchdog | 311 | 528 | **607** | 1097 | 1831 | 1/10（≤92ms） |
| Rust v4 | watchdog 先 `SW_HIDE` 再 layered | 329 | 556 | **624** | 1070 | 1759 | 1/10（≤93ms） |
| Rust v5 | layered 隐藏移到侧线程（不占关键路径） | 325 | 535 | **591** | 943 | 1725 | 0/10 |
| **Rust 最终（release，2×10 次）** | = v5 | 321 / 310 | 531 / 560 | **604 / 651** | 977 / 971 | 1663 / 1663 | 3/10、4/10（15–173ms） |
| **Rust v6（2×10 次）** | 检测后**立刻**建目标标签，不等源 URL | 325 / 325 | 522 / 531 | **358 / 354** | 381 / 385 | 1566 / 1555 | p50 0ms，最差 16ms |
| **Rust v7（当前 release，10 次）** | v6 + 隐藏位置恢复 / 首个窗口采纳 / 对话框早退 | 323 | 508 | **342** | 493 | 1506 | p50 0ms，p95 29ms，最差 44ms |

常驻开销（空闲、无待合并窗口时 10s 平均）：

| | 工作集 | 私有内存 | 空闲 CPU（单核占比） | 线程 | 文件体积 |
|---|---|---|---|---|---|
| C# ExplorerTabUtility | 134.0 MB | 105.6 MB | 1.25% | ~30 | 多文件 + .NET FW |
| Rust exptab (release) | **40.5 MB** | **10.7 MB** | 4.22% | 9 | **380 KB 单文件** |

注：Rust 的空闲 CPU 偏高来自 2ms 的窗口扫描轮询（可把 `detect_poll_ms` 调到 5–10ms
换取 <1% CPU，代价是隐藏窗口的起点推迟几毫秒）；C# 用 COM 事件驱动，但启动和内存代价大。

- C# 首次 bench（224815）作废：当时主窗口选择错误（`merged_ok=0`），不计入。
- Rust "最终"两轮为同一份 release 二进制，差异来自 Explorer 冷启动本身的高方差
  （见 §4），不代表代码差异。

## 3. 结论

1. **首标签出现时间：Rust v7 比 C# 快约 40%**（p50 342ms vs 574ms；p95 493ms vs 625ms），
   且尾部明显更稳。三轮独立 10 次测量一致（354 / 358 / 342ms）。
2. **提速来自调度顺序，不是更快的原语**：v5 是"等源窗口注册 → 读 URL → 建目标标签 →
   等目标标签注册 → Navigate2"，两段注册等待串行；v6 把"建目标标签"提到检测后 33ms，
   两段注册等待重叠，目标标签的注册只比源标签晚 20–45ms（见 §4.3）。合并链本身耗时没变。
3. **内容就绪（detect → Navigate2）≈600ms**（`metrics.csv` 的 `perceived_us`），绝对时间
   ≈ spawn+0.93s；这一段受 Explorer 自身"新建标签的浏览器初始化"限制，C# 走同一条
   `Navigate2` 路径、量级相同。要继续压缩只能绕开 COM 注册（UIA 读地址栏 + 设值/Invoke），
   复杂度与兼容性风险都高，v0.1 不做。
4. **Rust 的其它优势**：常驻私有内存 10.7MB（C# 105.6MB）、380KB 单文件无运行时依赖、
   检测比 C# 早 ~200ms（spawn+325ms vs C# 的 ~520ms）。
5. **Rust 的劣势**：空闲 CPU 4.2%（2ms 轮询，可调）；偶发 ≤16ms 闪现（2/10，C# 基线 0/10）；
   依赖 `0xA21B / 0xA021` 两个未公开命令号。
6. 早期"Rust 快 30–45%"的说法一度被 v1–v5 的实验否掉（那几版把优势消耗在"隐藏扰动
   Explorer 初始化"上），v6 用"先建标签"真正拿回了这部分时间。

## 4. 机理实验（为什么"更快"没兑现）

### 4.1 隐藏会推迟 Explorer 的 ShellWindows 注册

`metrics.csv`（本工具自己埋点，detect 为 0 点）：

| 版本 | detect→源窗口注册 | detect→新标签创建 | detect→目标标签可用 | detect→Navigate2 完成 |
|---|---|---|---|---|
| v1（detect 时同步隐藏） | 645–668 ms | 717 ms | 2194 ms | – |
| 不隐藏（nohide） | 215–341 ms | – | – | – |
| 最终版（延迟到注册后隐藏） | 225–232 ms | 228–236 ms | 716–746 ms | ~730–764 ms |

同步隐藏时源窗口注册从 ~230ms 涨到 ~650ms：`SetWindowLongPtrW(WS_EX_LAYERED)` 与
`SetWindowPos` 都是同步的跨进程调用，Explorer 的 UI 线程正在初始化该窗口，会被阻塞
到消息泵就绪为止（实测单次 90–220ms，最坏 500ms+）。注册被推迟，后续一切都被推迟。

结论：**检测可以早，但"改动窗口"必须晚**。最终版因此改成：

1. 检测到新窗口 → 立刻 `ShowWindowAsync(SW_HIDE)`（18µs，非阻塞，先把窗口按下去）；
2. 校验（单标签 / 开关 / Ctrl+Shift）→ 把任务交给合并线程；
3. 合并线程等到源窗口注册、读出 URL 之后，**另起线程**做 layered+移屏硬隐藏
   （此时 Explorer 已经初始化完该窗口，阻塞不再影响注册）；
4. 期间 watchdog（1ms 轮询 `IsWindowVisible`）负责在 Explorer 再次 `ShowWindow`
   时立即再按下去。

### 4.2 关源窗口

`0xA021 lParam=0`（C# 的"关窗口"命令）单发即可关闭隐藏窗口，但 Explorer 忙时会失败，
故加了三级兜底：`0xA021` → 400ms → `IWebBrowser2::Quit()` → 1.5s → `WM_CLOSE` → 800ms。
代价是 spawn→close 约 1.7s（C# 1.04s），但此时窗口早已不可见，用户无感；
不影响"标签出现"这一关键路径。

### 4.3 v6：提前建目标标签（v5 604ms → v6 358ms）

v5 的时序（metrics，相对 detect）：

| 阶段 | v5 | v6 |
|---|---|---|
| 目标标签子窗口出现 | 228–236 ms | **20–28 ms** |
| 源窗口注册（拿到 URL） | 225–232 ms | 550–930 ms（被并发建标签拖慢） |
| 目标标签注册（可 Navigate2） | 716–746 ms | 源注册 + 20–45 ms |
| Navigate2 完成（用户感知） | 730–764 ms | 566–620 ms |

v5 里两段等待是**串行**的：先等源窗口注册（~230ms）才去建标签，再等目标标签注册
（~480ms）。v6 在 detect+33ms 就建标签，Explorer 建这个标签会和"初始化源窗口"抢同一个
UI 线程，于是源窗口注册从 ~230ms 拖到 ~600ms —— 但目标标签的注册几乎与源标签同步到达，
两段等待被重叠，最终 `Navigate2` 提前了 ~130ms，而标签可见时间提前了 ~250ms。

代价：v5 里"隐藏不影响注册"的结论只在 v5 的顺序下成立；v6 中源窗口注册被拖慢，因此
"detect 后 95–200ms 的 layered 硬隐藏"改为在拿到源 URL 后的侧线程执行（不再压关键路径），
watchdog 的 `SW_HIDE` 仍负责第一时间压下窗口。闪现从 v5 的 15–173ms（3–4/10 次）
降到 p50 0ms、最差 16ms（2/10 次）。

### 4.4 其它已验证的事实

- `FindWindowExW` 扫顶层窗口 ~14µs；`EnumWindows`+过滤 1.2–1.9ms（慢 ~100 倍）。
- 2ms 轮询的常驻开销：空闲 10s CPU 422ms ≈ **4.2% 单核**（release，含 watchdog 的
  1ms 轮询；只在有待合并窗口时 watchdog 才启动）。
- `EVENT_OBJECT_CREATE` 对 `CabinetWClass` 迟到 0.7–2s，不能作为检测原语。
- `DwmSetWindowAttribute(DWMWA_CLOAK)` 对其它进程窗口返回 `E_ACCESSDENIED`，不可用。
- `ShowWindowAsync(SW_HIDE)` 会被 Explorer 在 ~1.6s 后 `ShowWindow` 覆盖，不能单独用。
- `IWebBrowser2::Navigate2` 跨进程调用必须传 4 个非空 `VARIANT`（否则
  `RPC_X_NULL_REF_POINTER 0x800706F4`）；标签 HWND → `IWebBrowser2` 只能靠
  `Count()/Item() → IServiceProvider → IShellBrowser::GetWindow` 映射；
  `IShellWindows::FindWindowSW` 不接受 HWND，本场景不可用。

## 5. 失败模式（metrics 的 `result` 列）

| result | 含义 | 处理 |
|---|---|---|
| `merged` | 成功 | 源窗口已关闭 |
| `merged_source_still_open` | 标签已并入，但三种方式都没关掉源窗口 | 窗口已隐藏；需用户手动关或进程退出 |
| `source_not_registered` | 源窗口在 `max_wait_ms` 内没注册进 ShellWindows | 放行（unhide），用户看到独立窗口 |
| `dest_registered` 超时 | 新标签没注册进 ShellWindows | 回滚（关闭新标签），放行源窗口 |
| `new_tab_timeout` | 主窗口没出现新标签 | 放行源窗口 |
| `navigate_failed` | `Navigate2` 返回错误 | 回滚标签，放行源窗口 |
| `source_dialog` | 该窗口弹出了 Explorer 的 #32770 对话框（无权限/位置不可用），永远不会注册 | 回滚标签，立刻放行（v7：实测隐藏 ~155ms） |
| `source_url_empty` | 拿到 IE 但 `LocationURL` 为空 | 回滚标签后放行（v7 起不再计为失败） |

任何失败路径都会 `unhide` 源窗口，且 v7 起 `unhide` 会**恢复窗口原来的屏幕位置**（v6 及
之前只去掉 `WS_EX_LAYERED`，窗口仍停在 -32000，表现为"窗口不见了"）。兜底命令：
`exptab rescue`。

### 5.1 v7 稳定性修复（用户实际试用后）

用户试用时遇到两个缺陷，均已修复并回归：

- **放行的窗口停在屏幕外**：`hide` 会把窗口移到 (-32000,-32000) 并置 layered alpha 0，
  但旧的 `unhide` 只去掉 layered、不还原位置；watchdog 还会在窗口放行后继续把它按下去。
  v7：`hide` 记录原始 `RECT`/layered/alpha，`unhide` 原样还原；worker→merger 的
  pending 集合让 watchdog 在窗口放行后立即退出；启动时与 `exptab rescue` 用默认位置
  恢复崩溃遗留窗口。
- **启动后第一个窗口被当成合并源**：daemon 启动时若系统里没有资源管理器窗口，主窗口为空，
  第一个新窗口被隐藏后找不到可并目标，最终被丢在屏幕外。v7：没有主窗口时**第一个新窗口
  自动成为主窗口并放行**（`no_main_window` 也会在合并线程里再兜底一次）。

另：访问无权限目录（Explorer 弹"你当前无权访问该文件夹"）的窗口永远不会进
`IShellWindows`，v6 会白白隐藏它 2–3s；v7 通过"该窗口是否有 owner 为它的 #32770 顶层
对话框"提前识别并放行（实测从 3020ms 降到 230ms）。

## 6. 原始数据

`tools\bench-out\merge-*.csv` / `-summary.json`：

- `merge-csharp-20261102-225014`（基线，10/10 合并成功）
- `merge-rust-20261102-224739`（v1，同步隐藏，10/10，零闪现）
- `merge-rustnohide-20261102-225159`（不隐藏对照）
- `merge-rust2-*` … `merge-rust5-*`（隐藏策略迭代，各 10 次）
- `merge-rust-final-20261102-225554`、`merge-rust-final2-20261102-225628`
  （v5 release 二进制，2×10 次）
- `merge-rustfast-20261103-065820`、`merge-rustfast2-20261103-070340`（v6 release，2×10 次，p50 354 / 358ms）
- `merge-rustfix-20261103-081946`（v7 release，10 次，p50 342ms / p95 493ms，含稳定性修复后回归）

## 7. v8：Win+E（虚拟文件夹窗口）合并

- 现象：Win+E 打开的 `主文件夹` 窗口不合并（`source_url_empty`）。
- 根因：Home 虚拟文件夹窗口的 `LocationURL` 为空，`Document.Folder.Self.Path` 为
  `::{F874310E-B6B7-47DC-BC84-B9E6B38F5903}`；且 `Navigate2` 传该字符串返回
  `E_INVALIDARG (0x80070057)`，必须传 `Folder` 自动化对象（VT_DISPATCH）。
- 修复：`com::location_of` 增加 `IShellFolderViewDual → Folder2 → Self → Path` 兜底；
  `com::navigate` 对 `::{...}`（及含 `#`/`%23`）路径改走
  `IShellDispatch::NameSpace(path) → Folder` + VT_DISPATCH `Navigate2`。
- 另：`daemon()` 起始用命名事件 `Local\exptab-rs-daemon` 做单实例仲裁（此前两个 daemon
  会同时跑、重复合并并写乱 `ipc.port`）。
- 实测（Release，pin scratch 主窗口 0x7a51b8e）：连续 4 次 Win+E 等价
  `Start-Process explorer.exe` → 全部 `merged`，`merges ok=4 failed=0`；
  每次 `navigate_us` 13.8–41.1ms，`total` 1.20–1.73s；合并出的 `主文件夹` 标签、
  15 秒后仍稳定存在，源窗口全部关闭。
- 未解决/注意：Home 窗口的 `source_registered` 比普通文件夹窗口更慢（0.9–1.5s），
  期间靠 `ShowWindowAsync(SW_HIDE)` 压制，理论上仍可能有极短闪现；此外测试过程中
  Explorer 有时会把已存在的 Home 窗口当作 Win+E 的复用处，导致该窗口被当作源窗口关闭。

## 8. v10：彻底消除闪屏 + 重复目录去重 + 开机自启

### 8.1 闪屏根因（用户实测 v7 反馈）

v7（以及更早版本）只在"检测到新窗口"时发一次 `ShowWindowAsync(SW_HIDE)`（约 18µs），
真正的"移出屏幕 + 全透明"隐藏推迟到源窗口注册之后（约 250-500ms）执行。Explorer 初始化
结束时会对窗口再调用一次 `ShowWindow`，于是窗口在"被重新显示"与"被彻底隐藏"之间存在
一段**真正在屏幕上的可见时间**；冷启动第一次打开 `explorer.exe` 初始化最慢，这段可见时间
最长，正是用户看到的"第一次打开能看到窗口闪一下"。之后的窗口偶尔"隐约可见"同理。

曾尝试但不可行的方案：`DwmSetWindowAttribute(DWMWA_CLOAK)` 跨进程 `E_ACCESSDENIED`；
同步 `SetWindowPos/SetWindowLongPtrW` 在 Explorer 初始化期阻塞 90-220ms，会推迟
`IShellWindows` 注册约 400ms（实测），反而拖慢合并。

### 8.2 v10 修复：检测瞬间异步移屏

```rust
win::hide_async(hwnd);   // 约 20µs
// SetWindowPos(hwnd, .., -32000, -32000, 0, 0,
//              SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS)
// ShowWindowAsync(hwnd, SW_HIDE)
```

`SWP_ASYNCWINDOWPOS` 把移动请求投递给窗口线程而**不等待**，所以它不会阻塞仍未开始泵消息的
Explorer 线程，却能在 Explorer 显示窗口之前就把窗口挪到屏幕外；之后无论 Explorer 怎么
`ShowWindow`，窗口都在屏幕外（watchdog 仍在窗口重新可见时补 `hide_async` + layered 兜底）。

**位置感知探针**（每 1ms 采样 `IsWindowVisible && GetWindowRect().Left > -30000`）
实测 10 次（v10 前后各 5 次）：`onscreen_at = -1`、`flash = 0ms`，即窗口**从未在屏幕上出现过**。
（bench-merge 的 flashed 列仍会显示 0-34ms，因为那只是 `IsWindowVisible` 标志——窗口此时在
-32000，用户看不到。）

### 8.3 重复目录去重（比 C# 更好用）

在"先建目标标签、并行等注册"的链路里，拿到源 URL 后先检查主窗口已有标签：
如果某个标签的 URL 与新窗口相同（且不是刚建的空白标签），则**不创建重复标签**，直接关掉
新窗口、保留用户原标签。`exptab metrics` 里该行 result=`deduped`，计入成功。
实测：同一目录连续打开两次 → 第 1 次 `merged`（`navigate_us` 67ms），第 2 次 `deduped`
（`hide_us` 31µs，未新增标签）。

### 8.4 易用性

- `exptab` 不带参数 = 确保后台 daemon 在跑（双击可用）；`exptab run` 才是前台。
- `exptab autostart on|off`：写/删 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
  下的 `exptab` 值（当前用户、无需管理员；值为 `"<exe路径>" start`）。已验证写入与删除。

### 8.5 v10 实测数字

| 指标 | C# 2.5.0 | Rust v7 | Rust v10 |
|---|---|---|---|
| 新标签出现 p50 | 574ms | 342ms | **408ms** |
| 真实屏幕闪屏 | 0ms（但其流程 hide 晚 217ms） | 0-44ms（可见标志） | **0ms（位置感知探针）** |
| 重复目录 | 产生重复标签 | 产生重复标签 | **去重（deduped）** |
| 空闲 CPU | 1.25% | 4.22% | 4.2%（2ms 轮询） |

v10 的 tab p50 比 v7 略高（408 vs 342ms）：为了在检测瞬间就把窗口移出屏幕，检测后立即做
了一次异步移动，Explorer 初始化被打断的时间点更早；但换来的是**完全无闪屏**，且仍比
C# 版快约 29%。
## 9. v11：第一次打开闪屏根因（采纳路径）与 Win+E 前台问题

### 9.1 "第一次打开闪一下，然后才显示出来"

- 根因：daemon 启动时系统里没有资源管理器窗口 → `main_hwnd = 0`。第一个新窗口在检测时
  被当作普通待合并源窗口 `hide_async` 隐藏；等标签建好（约 250-400ms）后才发现没有主窗口，
  于是把它"提升为主窗口"并 `unhide` 还原。用户看到的就是：窗口出现（隐藏前）→ 消失 ~0.3-1s
  → 重新出现。冷启动第一次打开最明显。
- 修复：worker 在检测时若 `main_hwnd == 0` 且没有其它带标签的窗口，则不隐藏
  （`adopt_candidate = true`），走采纳分支时也不需要 `unhide`。首窗打开体验与普通打开一致。
- 边界：如果两个窗口几乎同时打开，后一个会在发现主窗口已被占用时补一次 `hide_async` 再合并。

### 9.2 Win+E 后桌面看不到窗口、任务栏却显示已打开

- 根因：Win+E 的 Home 窗口被合并（或去重）进主窗口后，主窗口没有被激活/提到前台。
  如果用户当时在看别的应用，主窗口可能被遮挡或最小化，于是"看不到任何窗口"，而任务栏有图标。
- 修复：合并/去重成功后 `win::activate_window(main)`（`IsIconic → SW_RESTORE`；
  `SwitchToThisWindow(main, true)` 绕过前台锁；`SetForegroundWindow` 兜底）。
- 实测：Win+E 两次（一次 merged、一次 deduped）后 `GetForegroundWindow() == 主窗口`。

### 9.3 关于"闪屏"的测量口径

位置感知探针（每 1ms 采样 `IsWindowVisible && GetWindowRect().Left > -30000`）对
`explorer.exe /n,<dir>` 与 Win+E（Home）各 5 次：`onscreen_at = -1`、`flash = 0ms`，
即窗口从未出现在屏幕上。若用户仍看到闪烁，需区分：① 首次启动采纳路径（v11 已修）；
② 窗口被放行（多标签/Ctrl+Shift/无权限目录）时"先隐藏再还原"的短暂消失；
③ 主窗口被提到前台时的视觉变化。可结合 `exptab metrics` 的 `visible_at_detect` /
`result` 列定位。
## 10. v12：Win+E 多次后"窗口全部不可见"的根因与修复

### 10.1 现象

连按多次 Win+E 后，桌面没有任何资源管理器窗口，任务栏有图标；`GetForegroundWindow()`
指向别的程序。

### 10.2 现场证据

- `0x731340`（主窗口，`主文件夹`，1 标签）与 `0x1351c54`（备用 `主文件夹`，0 标签）：
  两者都是 `rect=(-32000,-32000)`、`IsWindowVisible=True`、`layered=False`。
- metrics：第一行 `no_main_window`（该窗口被隐藏后又被放行），随后 5 行都是 `deduped`。
- 因为主窗口本身在屏幕外，`activate_window()` 把"屏幕外的窗口"提到前台，用户仍看不到。

### 10.3 根因

1. 池化的 `主文件夹` 窗口被 Explorer 停在 (-32000,-32000)；`hide_async()` 在记录
   `Saved.rect` 时把这个屏幕外坐标当成了"原始位置"，`unhide()` 还原后仍在屏幕外。
2. `hidden_by_us()` 要求 `layered && alpha==0`，而这类窗口 `layered=False`，
   所以 `rescue_hidden()` 和启动自愈都不会处理它们。

### 10.4 修复

- `sane_rect()` 过滤无效坐标（≤ -30000、宽高 < 200）→ 使用默认位置 (120,90,1400,890)。
- `unhide()` 还原后再校验最终 rect，异常则强制默认位置。
- 1 秒自愈巡检：不属于 pending 任务的屏幕外 CabinetWClass 窗口一律 `unhide()`。
- 主窗口/候选窗口选择排除屏幕外窗口；`activate_window()` 先还原再激活。
- 去重/合并：先激活主窗口再关源窗口；去重时选中主窗口里已有的同 URL 标签。
- `close_own_tab()`：标签数 ≤ 1 时不发关闭命令（避免 Ctrl+W 语义关掉整个窗口），
  其余情况最多重试 4 次。

### 10.5 复测

- 8 次 Win+E（3.5s 间隔，从仅剩备用窗口的状态开始）：`merges ok=11 failed=0`，
  最终主窗口 visible、前台=主窗口、无屏幕外窗口。
- 干净启动（main=0）：第一次 Win+E 直接可见并被采纳为主窗口；随后 4 次 `deduped`。
- 普通目录 3 次合并：标签数每次 +1，位置感知探针 `onscreen_at=-1`，屏幕可见 0ms。

### 10.6 教训

- `0xA021 lParam=1` 的语义是 Ctrl+W：对**最后一个标签**发送会关闭整个窗口。
  工具内部关闭标签前必须 `tab_ids(main).len() > 1`。
- 任何"记录/还原窗口位置"的逻辑都必须对异常坐标做健康检查。

## 11. v13：新版 WinUI3/XAML 资源管理器窗口（不可关闭）的识别与保护

### 11.1 现象

用户截图：一个 `主文件夹` 窗口，标签栏有 2 个标签（第 2 个空白），内容区全白，
点 ×、Ctrl+W、点标签上的关闭按钮都没有反应——窗口无法关闭。

### 11.2 现场证据

- 窗口 `0x1351c54`：`CabinetWClass`，标题 `主文件夹 - 文件资源管理器`，
  visible=True，rect=(160,130)，pid=48860（explorer.exe shell 进程），
  style=0x14cf0000，exstyle=0x100；`IsHungAppWindow=False`，`SendMessageTimeout(WM_NULL)` 有响应。
- 子窗口：`TITLE_BAR_SCAFFOLDING_WINDOW_CLASS`、`InputNonClientPointerSource`、
  `Microsoft.UI.Content.DesktopChildSiteBridge`（内含 InputSiteWindowClass），
  **没有 `ShellTabWindowClass`**。
- UIA（Windows PowerShell 5.1 + `UIAutomationClient`/`UIAutomationTypes`）：
  `Microsoft.UI.Xaml.Controls.TabView` + 2 个 TabItem（第 2 个空白）+ 2 个"关闭标签页"按钮；
  `WindowPattern` 可用、`WindowInteractionState=ReadyForUserInteraction`、`IsModal=False`。
- 关闭尝试全部无效：`WM_CLOSE`、`WM_SYSCOMMAND(SC_CLOSE)`、UIA `WindowPattern.Close()`、
  UIA 关标签 `InvokePattern.Invoke()`、对 `TITLE_BAR_SCAFFOLDING_WINDOW_CLASS` 子窗口发关闭消息、
  `SetForegroundWindow` + Ctrl+W（前台切不过去，焦点在别的程序）。
- 最终处置：`taskkill /F /PID 48860` 杀掉 shell（AutoRestartShell=1 但 4s 内未自动重启），
  手动 `Start-Process explorer.exe`，新 shell pid=14288、`Shell_TrayWnd` 恢复，卡死窗口消失。

### 11.3 根因

这是 Windows 11 26H2（build 10.0.26300）的新版 **WinUI3/XAML** 文件资源管理器窗口：
外壳是 `CabinetWClass`，但标签栏整块是 XAML，没有 `ShellTabWindowClass` 子窗口。
旧版工具按经典窗口处理它（隐藏、移动、发经典标签消息），会把 XAML 内容弄成白板并使其拒绝关闭。

### 11.4 识别（build 26300 实测时间线）

经典窗口（`Start-Process explorer.exe /n,<dir>`，1ms 采样，5 次）：

| 事件 | 相对"首次被检测到" |
| --- | --- |
| 首次被检测到（窗口已在屏幕上） | 0ms（spawn 后 322–447ms） |
| `ShellTabWindowClass` 标签宿主出现 | +15–20ms |
| `IsWindowVisible=True` | +50–150ms |
| XAML 子窗口（`DesktopChildSiteBridge` / `TITLE_BAR_SCAFFOLDING...`）出现 | +450–480ms |

新版窗口相反：一起步就是 XAML，且**永远没有** `ShellTabWindowClass`。
因此分类器为 `is_new_style_explorer()` = 有 XAML 子窗口 **且** 没有经典标签宿主：
- 检测瞬间就有 XAML 子窗口 → 新版窗口，直接跳过（不隐藏、不合并）；
- 合并等待标签期间（60ms 后）出现"有 XAML、无经典标签宿主" → 判定为新版窗口，立即放行；
- 自愈巡检 / `rescue` 同样跳过它。

### 11.5 顺带修复：不碰 Explorer 的池化备用窗口

Explorer 的备用 `主文件夹` 窗口停在 (-32000,-32000) 但 `IsWindowVisible=True`。
v12 的 1 秒自愈会把它们搬回屏幕内，破坏 Explorer 的 Win+E 池。
现在只救"确实是我们藏的"窗口：`hidden_by_us()`（layered + alpha 0）或
"屏幕外 + 不可见"（`parked_by_us()`）；可见的池化窗口原样不动。

### 11.6 复测（v13 release，`target\release\exptab.exe` 410,624B）

- 4 次普通目录打开：位置感知探针（1ms 采样 `IsWindowVisible && rect.Left > -30000`）
  `onscreen_at` 为空 → 窗口从未出现在屏幕上，0ms 闪屏；全部 `merged`。
- 4 次 Win+E（Win+E 等价）：全部 `merged`/`deduped`，主窗口始终 visible、
  `GetForegroundWindow()==主窗口`，结束 `onscreen=8 offscreen=0 new_style=0`。
- metrics 修正：`detect_to_hide_us` 不再被"检测→注册后同步隐藏"的时间覆盖，
  恢复为真实的"检测→异步移屏"耗时（实测多为 100–500µs）。

### 11.7 教训

- `CabinetWClass` 不再等于"经典标签窗口"：26H2 起必须检查是否存在
  `ShellTabWindowClass`，否则会把新版 XAML 窗口弄坏（白板、无法关闭）。
- 对"Explorer 自己停到屏幕外的窗口"和"我们藏起来的窗口"要严格区分：
  只有后者可以 `unhide()`。

## 12. v14：Win+E 改为"此电脑"后窗口不可见（异步移屏竞态）

### 12.1 现象

用户把 Win+E 的目标从"主文件夹"改为"此电脑"
（`HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced\LaunchTo = 1`）后，
反复 Win+E 又出现"任务栏上看着打开了、桌面上没有窗口"。

### 12.2 现场证据

- 桌面无可见窗口，但枚举到两个 `CabinetWClass`：
  `0x4541954`、`0x1e9024a`，标题都是 '此电脑 - 文件资源管理器'，
  `IsWindowVisible = True`、`tabs = 1`、`rect = (-32000, -32000, -30750, -31200)`
  → 在屏幕外但处于"可见"状态。
- daemon（pid 54944）`main = 0x1e9024a`，`merges ok=11 failed=0 bypassed=5`；
  metrics 尾部两条均为 `no_main_window`（`0x4a2e6a`、`0x1e9024a`，
  `detect_to_hide_us` 52–258µs），随后 main 切到 0x1e9024a，但它仍在屏幕外。

### 12.3 根因

`hide_async()` 的 `SetWindowPos(-32000, -32000, SWP_ASYNCWINDOWPOS)` 只是把移动请求
**投递**给 Explorer 窗口线程；`no_main_window` 分支里 merger 线程紧接着同步 `unhide()`，
Explorer 稍后处理完那条排队消息又窗口挪回屏幕外，并且 `IsWindowVisible` 保持 True、
没有 layered 标志 → 用户看不见，`rescue`/自愈也判断不出是我们藏的。

稳定复现：开着 daemon，关掉主窗口，250ms 内 `Start-Process explorer.exe`
（LaunchTo=1 下等价于 Win+E）→ 新窗口最终停在 (-32000,-32000) 且 visible。

### 12.4 修复

- 新增 `place(hwnd, rect)`（同步 `SetWindowPos` + `SW_SHOWNA`）、
  `on_screen(hwnd)`、`settle_onscreen(hwnd, target)`（150ms 内每 5ms 复查，
  不在屏幕内或不可见就重新 `place`）。
- `unhide()` 重写：`place()` 到目标位置 → 再投递一次同位置的异步 `SetWindowPos`
  （排在待处理 park 之后，覆盖它）→ `settle_onscreen()` 兜底。

### 12.5 复测

- 3 轮"关主窗口 + 250ms 后打开"：全部以 1 个可见窗口结束（`off=False`，位置 (22,404)），
  无屏幕外残留。
- 两种 Win+E 模式兼容：Home 命名空间（`explorer.exe "/n,::{F874310E-B6B7-47DC-BC84-B9E6B38F5903}"`）
  → `merged`（新增"主文件夹"标签）；此电脑（`Start-Process explorer.exe`）→ `deduped`；
  再连开 3 次此电脑全部 `deduped`；`merges ok=7 failed=0 bypassed=4`，
  主窗口始终可见。

### 12.6 教训

- 跨进程 `SWP_ASYNCWINDOWPOS` 的"还原"必须考虑目标线程里尚未处理的异步移动消息：
  同步还原 + 立即重投递 + 短时间复查，才能避免"还原后又被打回屏幕外"。
- Win+E 的目标（Home / 此电脑）会改变新窗口的 URL 与注册时序，但合并/去重应按内容判断，
  不应依赖设置；两种模式共用同一条链路即可。

## 13. v15：重复打开的窗口不再"先建标签再关掉"

### 13.1 现象

主窗口已有"此电脑"（或某文件夹）标签时，再次 Win+E 会看到一个重复的新标签先出现
约 0.7–0.9s、随后消失（用户反馈"先出现我的电脑 tab，然后再消失"）。

### 13.2 根因

`merge_one` 为隐藏注册等待，先 `magic_new_tab()` 建目标标签，再并行等源窗口与新标签
注册（约 0.85–1.2s）；注册完成后才对比 URL 发现重复，调用 `close_own_tab()` 关掉自己
刚建的标签。新建标签默认显示 LaunchTo 目标（此电脑），所以这个"先建后关"的标签可见。

### 13.3 修复

- `wait_source_title(src, 400ms)`：轮询源窗口标题，直到出现 `" - "`（新窗口先显示通用
  应用名，80–100ms 后才变成 `<folder> - 文件资源管理器`）。
- `duplicate_hint(sw, main, source_title)`：遍历主窗口已有标签（`main_tabs.contains`），
  若 `<name> - 文件资源管理器` 的 `<name>` 恰好是已有标签标题 → 返回该标签
  `(tab_hwnd, url)` 作为候选（仅标题匹配，最终以 URL 校验为准）。
- 命中候选：`wait_source_entry(sw, src_tab, deadline)` 先等源窗口注册（不建标签）；
  `url.eq_ignore_ascii_case(&candidate_url)` → `select_tab_handle(main, tab_hwnd)` →
  `activate_window(main)` → `close_source(src, src_tab, src_ie)` → 返回 `deduped`；
  URL 不一致则继续走原合并流程（多等一次注册，不会误关）。
- `select_tab_handle(main, tab)`：0xA221 的参数是 1-based 视觉序号，而
  `children_of_class` 返回 Z-order（活动标签在最前，约等于视觉顺序反向）。先按
  `count - 1 - pos` 试一次并确认活动标签已切换，失败则 1..=count 逐个尝试
  （每次 `15ms × 8` 复查 `tab_ids(main)[0] == tab`）。
- `close_source()` 抽出源窗口关闭的三级兜底（`0xA021 lParam=0` → `IWebBrowser2::Quit`
  → `WM_CLOSE`），dedupe 与 merged 两条路径共用。

### 13.4 复测（release，daemon，主窗口 4–5 标签）

| 场景 | 标签数变化 | metrics |
| --- | --- | --- |
| 此电脑重复 ×3 | 4 → 4，采样 0 次变化 | `deduped`，`new_tab_us=0`，`perceived` 0.57–1.01s |
| 同一文件夹重复 ×3 | 4 → 4，0 次变化 | 同上 |
| 主文件夹命名空间重复 ×2 | 4 → 4，0 次变化 | 同上 |
| 新文件夹（对照） | 4 → 5，正常新增 | `merged`，`new_tab_us=0.50s`，`perceived 1.64s` |

去重时选中已有标签：主窗口标题实测从 `主文件夹 和 3 个其他选项卡` 切换为
`此电脑 和 3 个其他选项卡`，随后 `GetForegroundWindow() == 主窗口`。

### 13.5 教训

- "先建后关"的 UI 副作用无法靠加快关闭解决：正确做法是在创建前用便宜信号（窗口标题）
  预判，再用权威信号（注册后的真实 URL）确认；预判错误只多花一次等待，不会破坏数据。
- 0xA221 的序号是视觉序号，不是 HWND 枚举顺序；任何"按枚举顺序取下标"的假设都不可靠，
  需要"发送 → 验证活动标签"的闭环。
## 14. v16：空转（空闲）CPU —— 2ms 忙轮询的移除

### 14.1 现象

用户问："这个（CPU 对比里的数字）是软件启动后，不使用的情况下，一直占用这么多 CPU 吗？"
——是的。实测（`tools\idle-cpu.ps1`，按线程差分 `TotalProcessorTime`）用户实际运行 5 小时的
daemon：**空闲 CPU 2.18%–4.99% 的一个核心，且几乎全部集中在单一检测线程**（5s 采样 234ms）。

另有一次在同一二进制上只测到 0.94%。这个波动本身就是线索：它说明"2ms 轮询"的有效频率
取决于**系统当前定时器分辨率**（有没有别的程序调过 `timeBeginPeriod`），而不是我们的代码。

### 14.2 测量与根因

| 实验 | 结果 |
| --- | --- |
| `detect_poll_ms=50` | 0.47% 单核 |
| `detect_poll_ms=2` | 2.18% 单核 |
| `tools\wait-wakes.ps1`（`SetWaitableTimer(10ms)` + `WaitForSingleObject(ev, 10)`） | `wall=3005ms timer-wakes=194 event-wakes=0` → **64.6 次/秒** |
| 同一探针的事件唤醒次数 | 0（钩子当时只覆盖 create/show，前台切换不算） |

结论两条：
1. **`thread::sleep`/超时按 15.6ms tick 对齐**：请求 10ms 实际 15.6ms 醒一次；在此之前
   误以为"2ms 轮询"就是 2ms，实际上被系统拖到 15–31ms —— 于是检测延迟不稳定，而 CPU
   仍然按实际唤醒次数（每秒几十到几百次）花掉。
2. CPU 开销 ∝ 唤醒次数。单次唤醒（调度 + `FindWindowExW` 扫描 + 回睡）约 1.4ms 量级，
   乘以每秒几百次就是百分之一到几个百分点的核心占用。

### 14.3 修复

- **事件驱动等待**（`win::wait_detection_event`）：检测线程阻塞在
  `WaitForSingleObject(manual-reset event, tick)`。事件的唯一来源是一个
  `SetWinEventHook(EVENT_SYSTEM_FOREGROUND .. EVENT_OBJECT_SHOW, OUTOFCONTEXT |
  SKIPOWNPROCESS)` 回调：建窗、显示、前台切换都会 `SetEvent`。OUTOFCONTEXT 意味着
  回调跑在我们自己的线程里，**不注入 Explorer**。事件只是提示，醒来后仍用
  `FindWindowExW` 扫描确认，所以漏事件不会漏合并。
- **`detect_poll_ms` 2 → 20**：它现在只是"漏事件"的兜底（≈50 次/秒），不再是主路径；
  没有资源管理器窗口时用 `detect_poll_idle_ms=100`（每秒 10 次）。
- **`Heartbeat`（`CreateWaitableTimerW`）**：需要精确短睡时用它，绕过 15.6ms 对齐。
- **增量扫描**：`win::new_cabinet(&seen)` 常态下一次 `FindWindowExW` 返回 None；
  只有某个 `seen` 句柄消失时才全量重枚举（否则 after 锚点会漏检其后新建的窗口）。

### 14.4 复测（release，`detect_poll_ms=20` / `detect_poll_idle_ms=100`）

| 场景 | 旧 | 新 |
| --- | --- | --- |
| 空闲，无资源管理器窗口（15s × 3 轮） | 1.87% / 1.46% / 1.14% | **0.31%** |
| 空闲，有资源管理器窗口（15s / 12s × 2） | 1.04%（且更早为 2.73%） | **0.52% / 0.26% / 0.78%** |
| 一次 Win+E 期间总 CPU | — | 0.016s |

功能回归（同一构建）：

| 场景 | 结果 |
| --- | --- |
| 反复 Win+E（此电脑）×2 | `merges ok=2 failed=0 bypassed=0`，主窗口标签数不变、始终可见 |
| 新文件夹 alpha / beta | `merged`，标签 1→2、2→3，主窗口始终可见 |
| 闪屏（位置感知 1ms 探针） | `onscreen_at=-1`，0ms 真闪屏 |

### 14.5 顺带修掉的一个"窗口消失"形态

复测中发现主窗口 `IsWindowVisible() == false`、但位置完全正常（(277,387)）、无
`WS_EX_LAYERED`、无 alpha 层 —— 用户在任务栏能看到、桌面上什么都没有。原来的自愈
只认"屏幕外"或"layered + alpha 0"，这种形态两边都不匹配，所以救不回来。

修复：每秒巡检追加一条 —— 若 `main_hwnd` 存在、非 new-style、**不在屏幕外、不可见、
也不是我们藏的形态**，则 `ShowWindowAsync(SW_SHOWNA)` 把它显示回来。只对主窗口做，
且跳过 `pending_contains` 的窗口（合并过程中的源窗口本来就会短暂处于该状态）。

### 14.6 教训

- "2ms 轮询"在 Windows 上是**不可靠的语义**：真实频率由系统定时器分辨率决定，既不准
  也不省电。要么用 waitable timer，要么用事件。
- 事件钩子的 `min/max` 事件区间可以合法地跨越不同事件类别（这里 3 和 32768..32770），
  一个钩子就能同时覆盖"前台切换"和"窗口创建/显示"，比开两个钩子简单。
- 测量空闲 CPU 必须**按线程差分**并重复多轮：总量会随机器上其他程序的活动抖动几倍。
