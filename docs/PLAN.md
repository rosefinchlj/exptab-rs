# Rust 高性能资源管理器标签合并工具 — 实施方案

## 1. 目标

把 Windows 11 资源管理器"新开窗口"自动并入主窗口标签，做一个 **Rust 单文件常驻程序** 替换当前 C# 版
`ExplorerTabUtility 2.5.0`，重点优化"新窗口→变成标签"的感知延迟，并保证零可见闪烁。

第一版范围（已与用户确认）：**核心功能 = 新窗口自动并入主窗口标签 + 可开关**。

## 2. 现有 C# 实现的时间都花在哪（已读源码，仓库已 clone 到
`E:\use\ai\deepseek\Rust\ExplorerTabUtility`）

现有链路：`ShellWindows.WindowRegistered` 事件 → 等窗口对象（70ms 轮询，最多 2.5s）
→ 读 LocationURL → 隐藏源窗口（`EVENT_OBJECT_SHOW` + 分层 alpha=0）
→ 给主窗口发 `WM_COMMAND 0xA21B`（相当于 Ctrl+T）新建标签
→ 20ms 轮询等新标签 HWND → 50ms 轮询等该标签的 IE 对象（ShellWindows 集合里找）
→ `Navigate2(路径)` → 等 `NavigateComplete2`（最多 5s）→ `Quit` 源窗口。

可量化的浪费点（Rust 版逐个打掉）：

| # | 浪费点 | 现状 | Rust 方案 | 预期收益 |
|---|--------|------|-----------|----------|
| 1 | 等 ShellWindows 注册 | 70ms 轮询 + COM 事件投递 | 直接用 `SetWinEventHook(EVENT_OBJECT_CREATE/SHOW)` 拿 HWND，事件驱动 | 20–70ms |
| 2 | 跨进程枚举 ShellWindows 集合找 IE 对象 | 每轮全集合遍历 + QI | `IShellWindows::FindWindowSW(hwnd)` 单次调用拿到 `IWebBrowser2` | 10–50ms/次 |
| 3 | 等新标签出现 | 20ms 轮询 | WinEvent `EVENT_OBJECT_CREATE`（class=`ShellTabWindowClass`）立即触发 | 10–20ms |
| 4 | 重复导航 | 源窗口先完整导航一次，再关掉、再导航标签 | 读到 LocationURL 后立刻 `Stop()` 中止源窗口导航再关闭；标签只导航一次 | 大文件夹/网络路径省几百 ms |
| 5 | .NET/WPF/async 开销 | WPF 常驻 + COM 事件 + GC | 纯 Rust STA 消息循环，单线程事件驱动，无 GC/无 JIT | 启动与内存显著下降 |

## 3. 技术方案

### 3.1 检测（事件驱动，零轮询）
- `SetWinEventHook(EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, WINEVENT_OUTOFCONTEXT)`，消息循环线程接收。
- 过滤：class == `CabinetWClass`；排除自己；排除由非 explorer 进程创建的窗口（可选）。
- 在 `EVENT_OBJECT_SHOW` 时立即把新窗口设为分层 alpha=0（比 off-screen 更稳，不闪），
  若隐藏失败则 `SetWindowPos(SWP_HIDEWINDOW)` 兜底。
- 已知新窗口 HWND 后**不等**它注册完成，直接进入合并流程。

### 3.2 定位（一次 COM 调用）
- `CoInitializeEx(COINIT_APARTMENTTHREADED)`；`IShellWindows::FindWindowSW(new_hwnd, …)` → `IWebBrowser2`。
- `LocationURL` 读取目标；非文件系统命名空间（回收站/此电脑/控制面板）用
  `IWebBrowser2 → IServiceProvider → IShellBrowser → IShellView → IPersistFolder2::GetCurFolder` 拿 PIDL，
  `SHGetNameFromIDList(SIGDN_URL)` 转路径（作为兜底，先按 LocationURL 实现）。
- 只有"单标签的新窗口"才合并（多标签窗口视为用户主动新开）；`Ctrl+Shift` 按住时放行不合并。

### 3.3 并入
- 主窗口选择：优先"最老的、仍存活的 CabinetWClass 且单标签或多标签均可"的窗口（与 C# 的 `_mainWindowHandle`
  语义对齐）；主窗口消失后重新选。
- 用 `PostMessage(主窗口的 ShellTabWindowClass, WM_COMMAND, 0xA21B, 0)` 新建标签（已确认该 build
  的窗口存在 `ShellTabWindowClass` 子窗口，命令号待 P0 探针逐条验证）。
- WinEvent 收到新 `ShellTabWindowClass` 子窗口（parent = 主窗口）后：`FindWindowSW` 拿 `IWebBrowser2`
  → `Navigate2(path)`（或 PIDL 版本，兼容 `#`/含 `%23` 路径）。
- 不等 `NavigateComplete2`（不阻塞 UI），立即 `SetForegroundWindow(主窗口)`。
- 关掉源窗口：先 `IWebBrowser2::Stop`，再 `WM_CLOSE` / `IWebBrowser2::Quit`；清理隐藏标记。

### 3.4 开关 / 生命周期（无界面）
- 单实例：命名 Mutex + 命名管道命令通道。
- `exptab.exe`（默认）常驻；`exptab.exe toggle|on|off|status|quit` 通过命名管道操作运行中实例。
- 配置：`%LOCALAPPDATA%\exptab\config.ini`（纯 key=value，不引 serde，避免额外依赖）；
  键：`enabled`、`reuse_tab`(后续)、`exclude_processes`。
- 开机自启：README 提供注册表/HKCU Run 或计划任务说明（可选参数 `--install`/`--uninstall`）。

### 3.5 可观测性 / 埋点
每次合并写一行结构化日志（QPC 时间戳）：
`t_window_create → t_show → t_hidden → t_location_read → t_tab_created → t_navigate_posted → t_src_closed`，
`--log`/`--metrics` 输出到 `%LOCALAPPDATA%\exptab\metrics.csv`，用于和 C# 版对比。

## 4. 工程结构

```
exptab-rs/
  Cargo.toml           # 唯一依赖: windows 0.58（已验证 rsproxy 可拉取，构建 ~16s）
  src/main.rs          # 单实例、命令行、消息循环入口
  src/config.rs        # ini 解析/默认值/路径
  src/hooks.rs         # SetWinEventHook、窗口分类过滤、隐藏/还原
  src/com.rs           # STA 初始化、IShellWindows/FindWindowSW、IWebBrowser2、PIDL 兜底
  src/merge.rs         # 合并状态机（去重、超时、竞态处理）
  src/ipc.rs           # 单实例 + 命名管道命令
  src/metrics.rs       # QPC 埋点 + CSV
  tools/probe.ps1      # P0 可行性探针（验证魔法命令/COM 路径/事件顺序）
  tools/bench.ps1      # 对比压测：C# 版 vs Rust 版 各 N 次
  docs/PLAN.md
  README.md
```

## 5. 实施顺序与验收

**P0 可行性探针（先做，0.5h，风险最高的一步）**
在本机 build 26300 上验证：
1. `0xA21B` 新建标签、`0xA221` 切标签、关闭标签命令是否有效（在自建测试窗口上，不动用户现有窗口）；
2. `FindWindowSW(隐藏窗口 hwnd)` 能否拿到 `IWebBrowser2`，`LocationURL`/`Stop`/`Quit` 是否可用；
3. WinEvent 事件顺序与时间戳（create/show/tab-create/…）——决定是否需要调整挂接点。
验收：探针输出一张"能力矩阵 + 实测事件时间线"。

**P1 骨架**：单实例、WinEvent 检测、隐藏/还原、配置与命令行开关。

**P2 合并闭环**：发 0xA21B → 事件驱动捕获新标签 → Navigate2 → 关闭源窗口；无任何 sleep 轮询。

**P3 优化与稳定**：竞态（同时开多个窗口、用户中途关窗口、explorer 重启、拖出标签、主窗口关闭）、
`Stop` 源窗口、日志埋点、CPU/内存压测。

**P4 本机对比实测**：同一台机器、同样触发方式（`explorer.exe <path>` / 双击 / 中键），各 10 次，
记录 p50/p95：`create→tab 出现`、`create→导航发出`、`create→导航完成`、可见闪烁次数、常驻内存/CPU。

**P5 交付**：`target/release/exptab.exe` + 源码 + README（构建/使用/开关/自启/与 C# 版冲突提示）+ 对比报告。

**总验收标准**
- 单标签新窗口 100% 合并、无可见闪烁（压测 10 次无漏）；
- 端到端 `窗口创建 → 标签开始导航` p95 显著优于 C# 版（目标 ≤ 50%，基线实测后写死数字）；
- 常驻工作集 ≤ 10MB、空闲 CPU ≈ 0%、单文件 exe 无运行时依赖；
- `toggle/status/quit` 可用；退出后不残留隐藏窗口。

## 6. 风险与兜底

| 风险 | 兜底 |
|------|------|
| 魔法命令号（0xA21B 等）随 build 变化 | P0 探针先验证；失败退回 `SendInput` Ctrl+T 或 UIA（慢，但功能保底） |
| `FindWindowSW` 对标签 HWND 不支持 | 退回 `IShellWindows` 事件 + 一次枚举；再不行用 `IServiceProvider` 从标签窗口取 `IShellBrowser` |
| 无法阻止 Explorer 先创建源窗口 | 尽力做到"不可见 + 立即 Stop 导航"；若发现更早拦截点（注册表/CBT/注入）再评估升级 |
| 与 ExplorerTabUtility 冲突 | 启动时检测其进程，提示并拒绝并行；测试时临时退出 |
| 安全软件对全局 WinEvent hook 敏感 | 只做 OUTOFCONTEXT hook，不注入、不写 explorer 内存，风险最低 |
| 用户手速很快（连续开窗） | 合并队列串行化 + 每窗口去重 + 超时兜底（2s 未完成则放行显示原窗口） |

## 7. 测试约定（已获用户同意）

- 测试会短暂接管资源管理器：打开若干测试标签页（`C:\Windows`、`C:\Windows\System32` 等无害目录），
  测完自动关闭测试标签、恢复 ExplorerTabUtility。
- 测试期间临时退出 C# 版（结束进程），完成后重新启动 `D:\Program Files\ExplorerTabUtility\ExplorerTabUtility\ExplorerTabUtility.exe`。
- 全程不碰用户已打开的 4 个标签和已开窗口。
