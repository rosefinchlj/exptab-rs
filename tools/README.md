# 资源管理器「新窗口 → 变标签」延迟测量脚手架

用全局 WinEvent hook + 20 ms `EnumWindows` 轮询，测量一次
`explorer.exe /n,<dir>` 从**窗口出现**到**被合并进主窗口标签**再到**源窗口销毁**
的时间线，并汇总 p50 / p95。C# 版 ExplorerTabUtility 和 Rust 重写版用同一套脚本、
同一套判定，可直接做 A/B。

## 文件

| 文件 | 作用 |
| --- | --- |
| `WinEventTimeline.cs` | C# 时间线库，`Add-Type -TypeDefinition` 运行时编译。hook、QPC 时间戳、窗口/标签枚举、`WM_CLOSE`。 |
| `bench.ps1` | 测量驱动：基线快照 → 装 hook → 启动 explorer → 轮询判定 → CSV + p50/p95。 |
| `README.md` | 本文。 |

## 快速开始

```powershell
# 1) 只装 hook，验证脚本本身（不启动窗口、不碰任何窗口）
pwsh -NoProfile -File .\tools\bench.ps1 -DryRun

# 2) 单次冒烟（会真的开一个窗口；C# 工具在跑时会被合并）
pwsh -NoProfile -File .\tools\bench.ps1 -Runs 1 -DumpTimeline

# 3) 正式 10 次基准（等 lead 通知再跑）
pwsh -NoProfile -File .\tools\bench.ps1 -Runs 10

# 4) 固定路径（不推荐给开着 ReuseTabs 的 C# 版，见下）
pwsh -NoProfile -File .\tools\bench.ps1 -Runs 10 -UniqueSubdir:$false -Path 'C:\Windows\System32'
```

默认 CSV：`tools\bench-out\run-<yyyyMMdd-HHmmss>.csv`；`-DumpTimeline` 额外写
`<csv名>-runNN-timeline.csv` 并在控制台打印第 1 次的原始时间线。

## 工作原理

1. **基线（每次 run 前）**：枚举所有 `CabinetWClass` 顶层窗口及其直接子窗口
   `ShellTabWindowClass`（标签是顶层窗口的直接子窗口），记下 HWND 集合。
2. **装 hook**：`SetWinEventHook(0x8000..0x8003, WINEVENT_OUTOFCONTEXT |
   WINEVENT_SKIPOWNPROCESS)`，过滤 `idObject == OBJID_WINDOW (0)`；回调里先取
   `QueryPerformanceCounter`，再做跨进程查询。只记录类名为 `CabinetWClass` /
   `ShellTabWindowClass` 的事件。
3. **触发**：`Process.Start(%windir%\explorer.exe, "/n,\"<dir>\"")`，返回启动前的
   QPC。每个 run 默认在 `%TEMP%\exptab-bench-probe\run-<stamp>-NN` 新建一个空目录
   （`-UniqueSubdir`，默认开），确保每次都是**新路径**。
4. **判定轮询（20 ms）**：每轮同时做
   - `EnumWindows` 快照：新 `CabinetWClass` 的首次出现时间（`first_seen`）、
     首次可见时间、首次 alpha=0 / 不可见时间、首次消失时间；基线窗口下新出现的
     `ShellTabWindowClass` 的首次出现时间；
   - hook 快照：create/show/hide/destroy 事件。
   两者取**更早**的观测作为该事件时刻（见「已知误差」）。
5. **结束条件**：源窗口已隐藏/销毁 **且** 已出现新标签；或 5 s 超时。合并判定后
   再等最多 `-GraceMs`（默认 1500 ms）让源窗口销毁，避免 `t_create_to_destroy`
   被截断。

### 为什么创建时间要轮询

实测（本机 Win11 26300）Explorer 的 `EVENT_OBJECT_CREATE` 会**晚到 0.3–2 s**，
甚至晚于 `EVENT_OBJECT_SHOW`，而回调本身耗时只有几微秒（时间线里的 `proc_us`）。
因此脚本把「`EnumWindows` 首次看到该 HWND」也当作创建时刻，取二者最小值。
CSV 的 `create_source` / `tab_source` 会写 `event` 或 `poll`，便于事后判断这次
用的哪个时钟。

### 状态判定（`status` 列）

| status | 含义 |
| --- | --- |
| `merged` | 出现新标签 **且** 源窗口隐藏/销毁 = 合并成功（`succeeded=True`） |
| `tab-no-hide` | 有新标签，但源窗口在超时前一直可见 |
| `hidden-no-tab` | 源窗口隐藏/销毁了，但基线窗口下没出现新标签 |
| `timeout` | 5 s 内既没新标签也没隐藏（没有工具在跑时的正常结果） |
| `no-window` | 连新 `CabinetWClass` 都没看到（explorer 启动失败/被复用） |

## 分别测 C# 版和 Rust 版

**C# 基线**

- 确认 `ExplorerTabUtility.exe` 在跑（脚本会打印 PID；不在跑会提示，此时行应为
  `timeout`）。
- 保持默认 `-UniqueSubdir`。C# 的 `ReuseTabs=true` 在路径已打开时会**选中已有
  标签并 Quit 源窗口，不会创建新标签**；用固定 `-Path` 连跑第二次开始就会
  变成 `hidden-no-tab`，数字不可比。
- 测量期间不要重启/改设置；每次重启都会改变它跟踪的窗口集合。

**Rust 版**

- 由 lead 停掉 C# 进程、启动 Rust 版（本脚本不 kill 任何进程）。
- 命令完全相同：`.\bench.ps1 -Runs 10`。`merged` 比例和 p50/p95 直接对比。
- 若 Rust 版不做 ReuseTabs，可以用 `-UniqueSubdir:$false -Path ...`，但两版必须
  用同一种模式，否则不是同一场景。

**公平性要点**：同一台机器、同一时段、同一 `-Path` 语义（建议都用默认
`-UniqueSubdir`）、同样先让桌面空闲；不要一边编译一边测。

## 安全约束（脚本已实现）

- 只关闭**本次 run 新建**、且不在基线里的 `CabinetWClass`；关闭前校验属主进程是
  `explorer.exe`，且标题含本次探针目录名或窗口已 alpha=0。
- **从不关闭标签**，包括合并后留在主窗口里的探针标签；也从不关闭基线窗口。
- `-DryRun` 不启动 explorer、不发任何窗口消息。
- 没有工具在跑时，`timeout` 之后由脚本用 `WM_CLOSE` 关掉自己开的窗口。

## 已知误差 / 局限

1. **创建时刻**：`EVENT_OBJECT_CREATE` 可能晚到数百 ms～2 s；已用轮询兜底，但
   轮询周期 20 ms + `EnumWindows` 开销（约 1–4 ms），`create_us` 仍有约 ±25 ms
   不确定性。`create_source` 标明来源。
2. **隐藏时刻**：C# 的 `HideWindow` 调 `SetLayeredWindowAttributes(alpha=0)`，
   **不触发** `EVENT_OBJECT_HIDE`，且 `IsWindowVisible` 仍为 true；只能轮询
   `GetLayeredWindowAttributes` 发现，误差上界约一个轮询周期（+开销）。
3. **新标签时刻**：标签 create 事件同样可能晚到（实测约 0.2–0.3 s），取轮询与
   事件较早者；轮询看到的是标签 HWND 已存在，不保证已开始导航。
4. **可见时长 `flashed_ms`** = 隐藏时刻 − 首次 show（无 show 则用创建）。若窗口
   创建后先不可见、再 show，这是实际可见区间；若 show 事件晚到，会偏小。
5. **销毁时刻**：实现若在合并后仍保留隐藏窗口超过 `-GraceMs`，`t_create_to_destroy_us`
   为空（脚本随后清理，但清理耗时**不计入**）。C# 的 `window.Quit()` 是 COM 调用，
   可能比新标签晚数百 ms。
6. **采样数**：`-Runs 10` 时 p95 用 nearest-rank，等于第 10 个（最大）值；它只是
   指示性的，别当稳健分位数。建议报 p50 + 成功率 + 最大值。
7. **环境噪声**：期间用户手动开关窗口/标签、或另一个探针/基准（例如 lead 的
   Rust 探针）同时在操作 Explorer，都会被当成 run 的一部分（源窗口取「最早出现的
   新窗口」）；测试时保持桌面空闲、只留一个资源管理器窗口，且不要和别的测试并发。
   每个 run 前先看基线窗口数；数字突然漂移就丢弃该批。
8. **工具重启**：ExplorerTabUtility 在 run 之间重启会改变基线/行为；同一批数据必须
   在进程稳定的时间段内采。
9. **标签归属**：判定只要求新标签的 parent 是**任一**基线窗口；C# 合并到它认为的
   主窗口，一般就是唯一那个窗口。多窗口场景下不区分「主」窗口。
10. **`-Path` 语义**：`-UniqueSubdir` 开启时 `-Path` 被忽略（会提示）；这是为了
    绕开 ReuseTabs，不是 bug。

## 文件格式

### 时间线 CSV（`-DumpTimeline`）

`t_us,evt,class,hwnd,parent,pid,visible,proc_us,note`

- `t_us`：相对 `Start()` 的微秒，在 hook 回调入口取；
- `evt`：`create` / `destroy` / `show` / `hide` / `mark`；
- `proc_us`：该回调从取时间到处理完的耗时（诊断 hook 是否被拖慢）；
- `note`：`mark` 的名字（如 `spawn`）。

### 汇总 CSV（每 run 一行）

`run,target,status,succeeded,source_hwnd,source_pid,tab_hwnd,tab_parent,spawn_us,
create_us,create_source,show_us,hide_us,hide_source,destroy_us,
t_create_to_hide_us,t_create_to_new_tab_us,t_create_to_destroy_us,flashed_ms,
windows_before,tabs_before,tab_source,note`

- 时间为微秒（`spawn_us` 起算），`flashed_ms` 为毫秒；
- `hide_source`：`event` / `poll-alpha0` / `poll-invisible` / `destroy`；
- `note`：未合并时的人话原因。

## 冒烟记录（2026-11-02，本机）

- `-DryRun`：hook 正常装卸，2 个 mark 间隔 1205 ms，0 个窗口事件。
- 合并成功样例（C# 在跑，`run-20261102-222753.csv`）：`merged`，
  `create_us=368402`（poll），`show=475529`，`hide=632035`（poll-alpha0），
  新标签 `0x826E4` parent `0xE019A8`，`t_create_to_hide=263.6 ms`，
  `t_create_to_new_tab=378.2 ms`，`flashed_ms=156.5`。
- 工具停掉的样例（`run-20261102-222849.csv`）：`hidden-no-tab`（外部把窗口关了），
  说明脚本在无工具时也能走完判定，不会误报 `merged`。
