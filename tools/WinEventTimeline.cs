// WinEventTimeline.cs
// -----------------------------------------------------------------------------
// Minimal, dependency-free timeline recorder for "new Explorer window -> tab"
// measurements.  It is meant to be compiled at runtime by PowerShell:
//
//     Add-Type -TypeDefinition (Get-Content .\WinEventTimeline.cs -Raw)
//
// Design notes
//  * One out-of-context WinEvent hook covers EVENT_OBJECT_CREATE (0x8000),
//    EVENT_OBJECT_DESTROY (0x8001), EVENT_OBJECT_SHOW (0x8002) and
//    EVENT_OBJECT_HIDE (0x8003).  WINEVENT_SKIPOWNPROCESS keeps PowerShell's
//    own UI out of the trace.
//  * Only window objects (idObject == OBJID_WINDOW) of the two classes we care
//    about are recorded: CabinetWClass (an Explorer window) and
//    ShellTabWindowClass (an Explorer tab; its parent is the window).
//  * Event delivery is asynchronous, so timestamps are the QPC time at which
//    the hook callback runs, not the kernel event time.  See README for the
//    error budget.
//  * Destroy notifications may arrive after the HWND is unusable, so the class
//    name is cached on create/show and looked up from the cache on destroy.
//  * No UI, no threads except the hook thread, no external packages.
// -----------------------------------------------------------------------------

using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

/// <summary>One recorded timeline event.</summary>
public sealed class TimelineEvent
{
    /// <summary>Microseconds since Start(), sampled at hook-callback entry.</summary>
    public double t_us;
    /// <summary>Microseconds spent inside the callback after the timestamp (diagnostic).</summary>
    public double proc_us;
    /// <summary>"create", "destroy", "show", "hide" or "mark".</summary>
    public string evt;
    /// <summary>Window class name ("CabinetWClass" / "ShellTabWindowClass"); empty for marks.</summary>
    public string cls;
    /// <summary>HWND as a 64-bit integer (0 for marks).</summary>
    public long hwnd;
    /// <summary>Parent HWND (for a tab this is the owning CabinetWClass window).</summary>
    public long parent;
    /// <summary>Owning process id (0 when the window is already gone).</summary>
    public int pid;
    /// <summary>IsWindowVisible() at callback time.</summary>
    public bool visible;
    /// <summary>Mark name, or empty.</summary>
    public string note;

    public override string ToString()
    {
        string n = string.IsNullOrEmpty(note) ? string.Empty : "  note=" + note;
        return string.Format(
            "{0,12:F1} us  {1,-7}  {2,-22}  hwnd=0x{3:X}  parent=0x{4:X}  pid={5,-6} vis={6}  proc={7:F0}us{8}",
            t_us, evt, cls, hwnd, parent, pid, visible ? "1" : "0", proc_us, n);
    }
}

/// <summary>Snapshot of one top-level Explorer window.</summary>
public sealed class WindowInfo
{
    public long hwnd;
    public long parent;
    public int pid;
    public bool visible;
    /// <summary>WS_EX_LAYERED alpha (0..255), or -1 when the window is not layered / query failed.</summary>
    public int layered_alpha;
    public string cls;
    public string title;
    /// <summary>Direct ShellTabWindowClass children (tabs).</summary>
    public long[] tabs;

    public int tab_count
    {
        get { return tabs == null ? 0 : tabs.Length; }
    }

    public override string ToString()
    {
        return string.Format(
            "hwnd=0x{0:X} pid={1} vis={2} alpha={3} tabs={4} '{5}'",
            hwnd, pid, visible ? "1" : "0", layered_alpha, tab_count, title);
    }
}

/// <summary>
/// Static recorder.  Typical use from PowerShell:
///   [WinEventTimeline]::Start()
///   $t0 = [WinEventTimeline]::SpawnExplorer('C:\Windows\System32')
///   ...
///   $events = [WinEventTimeline]::StopAndDump()
///   $events | Format-Table
/// </summary>
public static class WinEventTimeline
{
    private const string WindowClass = "CabinetWClass";
    private const string TabClass = "ShellTabWindowClass";

    private const uint EVENT_OBJECT_CREATE = 0x8000;
    private const uint EVENT_OBJECT_DESTROY = 0x8001;
    private const uint EVENT_OBJECT_SHOW = 0x8002;
    private const uint EVENT_OBJECT_HIDE = 0x8003;

    private const uint WINEVENT_OUTOFCONTEXT = 0x0000;
    private const uint WINEVENT_SKIPOWNPROCESS = 0x0002;

    private const uint WM_CLOSE = 0x0010;
    private const uint WM_QUIT = 0x0012;
    private const uint PM_NOREMOVE = 0x0000;

    private const int OBJID_WINDOW = 0;

    // ---- state -------------------------------------------------------------

    private static readonly object _lock = new object();
    private static readonly List<TimelineEvent> _events = new List<TimelineEvent>();
    private static readonly Dictionary<long, string> _classCache = new Dictionary<long, string>();

    // Only touched by the hook thread.
    private static readonly StringBuilder _hookSb = new StringBuilder(512);
    private static readonly WinEventDelegate _callback = new WinEventDelegate(OnWinEvent);

    private static readonly ManualResetEvent _ready = new ManualResetEvent(false);
    private static Thread _thread;
    private static uint _threadId;
    private static IntPtr _hook = IntPtr.Zero;
    private static volatile bool _running;
    private static string _startError;

    private static long _freq;
    private static long _startQpc;

    // ---- public API --------------------------------------------------------

    public static bool IsRunning
    {
        get { return _running; }
    }

    /// <summary>QPC frequency (ticks per second) captured by Start().</summary>
    public static long Frequency
    {
        get { return _freq; }
    }

    public static long StartQpc
    {
        get { return _startQpc; }
    }

    /// <summary>Starts a fresh recording: clears events, installs the hook, starts the hook thread.</summary>
    public static void Start()
    {
        lock (_lock)
        {
            if (_running)
                throw new InvalidOperationException("WinEventTimeline is already running.");

            _events.Clear();
            _classCache.Clear();
            _startError = null;
            _startQpc = 0;
            QueryPerformanceFrequency(out _freq);
            QueryPerformanceCounter(out _startQpc);

            _ready.Reset();
            _running = true;
            _thread = new Thread(HookThread);
            _thread.IsBackground = true;
            _thread.Name = "WinEventTimeline.Hook";
            _thread.Priority = ThreadPriority.AboveNormal;
            _thread.Start();
        }

        if (!_ready.WaitOne(3000))
        {
            _running = false;
            throw new InvalidOperationException("Timed out while installing the WinEvent hook.");
        }
        if (_startError != null)
        {
            _running = false;
            string err = _startError;
            _startError = null;
            throw new InvalidOperationException(err);
        }
    }

    /// <summary>Stops the hook.  Safe to call when not running.</summary>
    public static void Stop()
    {
        Thread t;
        uint tid;
        lock (_lock)
        {
            if (!_running && _thread == null)
                return;
            _running = false;
            t = _thread;
            tid = _threadId;
        }

        if (tid != 0)
            PostThreadMessage(tid, WM_QUIT, IntPtr.Zero, IntPtr.Zero);
        if (t != null)
            t.Join(2000);

        lock (_lock)
        {
            _thread = null;
            _threadId = 0;
        }
    }

    /// <summary>Stops recording and returns a copy of all events.</summary>
    public static TimelineEvent[] StopAndDump()
    {
        Stop();
        return Snapshot();
    }

    /// <summary>Thread-safe copy of the events recorded so far.</summary>
    public static TimelineEvent[] Snapshot()
    {
        lock (_lock)
        {
            TimelineEvent[] copy = new TimelineEvent[_events.Count];
            _events.CopyTo(copy);
            return copy;
        }
    }

    /// <summary>Adds a named marker to the timeline (evt = "mark").</summary>
    public static void Mark(string name)
    {
        AddEvent(UsSinceStart(NowQpc()), "mark", string.Empty, 0, 0, 0, false, name);
    }

    /// <summary>
    /// Starts "explorer.exe /n,<path>" and returns the QPC timestamp taken just
    /// before Process.Start.  Convert with UsSinceStart().
    /// </summary>
    public static long SpawnExplorer(string path)
    {
        long t0 = NowQpc();
        string clean = (path ?? string.Empty).Replace("\"", string.Empty);
        string exe = null;
        try
        {
            exe = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows), "explorer.exe");
        }
        catch
        {
            exe = null;
        }
        if (string.IsNullOrEmpty(exe) || !File.Exists(exe))
            exe = "explorer.exe";

        ProcessStartInfo psi = new ProcessStartInfo(exe, "/n,\"" + clean + "\"");
        psi.UseShellExecute = false;
        try
        {
            Process p = Process.Start(psi);
            if (p != null)
                p.Dispose();
        }
        catch (Exception ex)
        {
            throw new InvalidOperationException("Failed to start explorer.exe: " + ex.Message, ex);
        }
        return t0;
    }

    /// <summary>Posts WM_CLOSE (never SendMessage, so a hung window cannot block us).</summary>
    public static bool CloseWindow(long hwnd)
    {
        if (hwnd == 0)
            return false;
        return PostMessage(new IntPtr(hwnd), WM_CLOSE, IntPtr.Zero, IntPtr.Zero);
    }

    /// <summary>Current top-level CabinetWClass windows with their tab HWNDs.</summary>
    public static WindowInfo[] GetExplorerWindows()
    {
        List<WindowInfo> result = new List<WindowInfo>();
        EnumWindows(delegate(IntPtr hwnd, IntPtr lParam)
        {
            if (ClassOf(hwnd) == WindowClass)
                result.Add(BuildWindowInfo(hwnd));
            return true;
        }, IntPtr.Zero);
        return result.ToArray();
    }

    /// <summary>Direct ShellTabWindowClass children of a window (empty if hwnd is not alive).</summary>
    public static long[] GetTabs(long hwnd)
    {
        List<long> tabs = new List<long>();
        if (hwnd == 0)
            return tabs.ToArray();
        IntPtr root = new IntPtr(hwnd);
        if (!IsWindow(root))
            return tabs.ToArray();

        EnumChildWindows(root, delegate(IntPtr child, IntPtr lParam)
        {
            if (GetParent(child) == root && ClassOf(child) == TabClass)
                tabs.Add(child.ToInt64());
            return true;
        }, IntPtr.Zero);
        return tabs.ToArray();
    }

    /// <summary>Snapshot of one window, or null when it is not an existing window.</summary>
    public static WindowInfo GetWindowInfo(long hwnd)
    {
        if (hwnd == 0)
            return null;
        IntPtr h = new IntPtr(hwnd);
        if (!IsWindow(h))
            return null;
        return BuildWindowInfo(h);
    }

    public static bool IsWindowAlive(long hwnd)
    {
        return hwnd != 0 && IsWindow(new IntPtr(hwnd));
    }

    public static bool IsWindowVisible(long hwnd)
    {
        return hwnd != 0 && IsWindowVisibleNative(new IntPtr(hwnd));
    }

    /// <summary>Layered-window alpha (0..255), or -1 when not layered / not queryable.</summary>
    public static int GetLayeredAlpha(long hwnd)
    {
        if (hwnd == 0)
            return -1;
        uint key;
        byte alpha;
        uint flags;
        if (!GetLayeredWindowAttributes(new IntPtr(hwnd), out key, out alpha, out flags))
            return -1;
        return (int)alpha;
    }

    /// <summary>Human readable one-line description of a HWND (class/title/pid/alpha/tabs).</summary>
    public static string Describe(long hwnd)
    {
        if (hwnd == 0)
            return "hwnd=0";
        IntPtr h = new IntPtr(hwnd);
        if (!IsWindow(h))
            return string.Format("0x{0:X} <not a window>", hwnd);
        return BuildWindowInfo(h).ToString();
    }

    /// <summary>Formats an event array as CSV text (for logs / console dumps).</summary>
    public static string DumpText(TimelineEvent[] events)
    {
        if (events == null)
            events = new TimelineEvent[0];
        StringBuilder sb = new StringBuilder();
        sb.AppendLine("t_us,evt,class,hwnd,parent,pid,visible,proc_us,note");
        for (int i = 0; i < events.Length; i++)
        {
            TimelineEvent e = events[i];
            sb.AppendFormat("{0:F1},{1},{2},0x{3:X},0x{4:X},{5},{6},{7:F0},{8}",
                e.t_us, e.evt, e.cls, e.hwnd, e.parent, e.pid, e.visible ? 1 : 0, e.proc_us, e.note).AppendLine();
        }
        return sb.ToString();
    }

    // ---- clock -------------------------------------------------------------

    public static long NowQpc()
    {
        long value;
        QueryPerformanceCounter(out value);
        return value;
    }

    public static double UsSinceStart(long qpc)
    {
        long f = _freq;
        if (f == 0)
            return 0.0;
        return (qpc - _startQpc) * 1000000.0 / f;
    }

    // ---- hook plumbing -----------------------------------------------------

    private static void HookThread()
    {
        _threadId = GetCurrentThreadId();
        MSG msg;
        // Force creation of this thread's message queue before installing the hook.
        PeekMessage(out msg, IntPtr.Zero, 0, 0, PM_NOREMOVE);

        _hook = SetWinEventHook(EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE, IntPtr.Zero,
            _callback, 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        if (_hook == IntPtr.Zero)
        {
            _startError = "SetWinEventHook failed (Win32 error " +
                Marshal.GetLastWin32Error().ToString() + ").";
            _ready.Set();
            return;
        }

        _ready.Set();

        while (GetMessage(out msg, IntPtr.Zero, 0, 0) > 0)
        {
            TranslateMessage(ref msg);
            DispatchMessage(ref msg);
        }

        if (_hook != IntPtr.Zero)
        {
            UnhookWinEvent(_hook);
            _hook = IntPtr.Zero;
        }
    }

    private static void OnWinEvent(IntPtr hWinEventHook, uint eventType, IntPtr hwnd,
        int idObject, int idChild, uint dwEventThread, uint dwmsEventTime)
    {
        if (!_running || hwnd == IntPtr.Zero)
            return;
        // Window-level notifications only; this cheap check discards the bulk of
        // UI-element traffic before we pay for a cross-process class lookup.
        if (idObject != OBJID_WINDOW)
            return;

        // Timestamp first: the lookups below cross process boundaries and can
        // block while the target thread is still busy creating the window.
        double tEntry = UsSinceStart(NowQpc());

        long h = hwnd.ToInt64();
        string cls = null;
        lock (_lock)
        {
            string cached;
            if (_classCache.TryGetValue(h, out cached))
                cls = cached;
        }

        if (cls == null)
        {
            int len = GetClassName(hwnd, _hookSb, _hookSb.Capacity);
            if (len <= 0)
                return; // already destroyed and never seen: cannot classify
            cls = _hookSb.ToString();
            if (cls != WindowClass && cls != TabClass)
                return;
            lock (_lock)
            {
                _classCache[h] = cls;
            }
        }

        long parent = GetParent(hwnd).ToInt64();
        uint pid;
        GetWindowThreadProcessId(hwnd, out pid);
        bool visible = IsWindowVisibleNative(hwnd);

        AddEvent(tEntry, EventName(eventType), cls, h, parent, unchecked((int)pid), visible, null);

        if (eventType == EVENT_OBJECT_DESTROY)
        {
            lock (_lock)
            {
                _classCache.Remove(h);
            }
        }
    }

    private static string EventName(uint eventType)
    {
        switch (eventType)
        {
            case EVENT_OBJECT_CREATE: return "create";
            case EVENT_OBJECT_DESTROY: return "destroy";
            case EVENT_OBJECT_SHOW: return "show";
            case EVENT_OBJECT_HIDE: return "hide";
            default: return "event-0x" + eventType.ToString("X");
        }
    }

    private static void AddEvent(double tUs, string evt, string cls, long hwnd, long parent,
        int pid, bool visible, string note)
    {
        TimelineEvent e = new TimelineEvent();
        e.t_us = tUs;
        e.proc_us = Math.Max(0.0, UsSinceStart(NowQpc()) - tUs);
        e.evt = evt;
        e.cls = cls ?? string.Empty;
        e.hwnd = hwnd;
        e.parent = parent;
        e.pid = pid;
        e.visible = visible;
        e.note = note ?? string.Empty;
        lock (_lock)
        {
            _events.Add(e);
        }
    }

    // ---- window helpers ----------------------------------------------------

    private static WindowInfo BuildWindowInfo(IntPtr hwnd)
    {
        WindowInfo wi = new WindowInfo();
        wi.hwnd = hwnd.ToInt64();
        wi.cls = ClassOf(hwnd);
        wi.parent = GetParent(hwnd).ToInt64();
        uint pid;
        GetWindowThreadProcessId(hwnd, out pid);
        wi.pid = unchecked((int)pid);
        wi.visible = IsWindowVisibleNative(hwnd);
        wi.layered_alpha = GetLayeredAlpha(wi.hwnd);
        wi.title = TitleOf(hwnd);
        wi.tabs = GetTabs(wi.hwnd);
        return wi;
    }

    private static string ClassOf(IntPtr hwnd)
    {
        StringBuilder sb = new StringBuilder(256);
        int len = GetClassName(hwnd, sb, sb.Capacity);
        return len > 0 ? sb.ToString() : string.Empty;
    }

    private static string TitleOf(IntPtr hwnd)
    {
        int len = GetWindowTextLength(hwnd);
        if (len <= 0)
            return string.Empty;
        StringBuilder sb = new StringBuilder(len + 1);
        GetWindowText(hwnd, sb, sb.Capacity);
        return sb.ToString();
    }

    // ---- native interop ----------------------------------------------------

    [UnmanagedFunctionPointer(CallingConvention.Winapi)]
    private delegate void WinEventDelegate(IntPtr hWinEventHook, uint eventType, IntPtr hwnd,
        int idObject, int idChild, uint dwEventThread, uint dwmsEventTime);

    private delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential)]
    private struct MSG
    {
        public IntPtr hwnd;
        public uint message;
        public IntPtr wParam;
        public IntPtr lParam;
        public uint time;
        public int ptX;
        public int ptY;
    }

    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SetWinEventHook(uint eventMin, uint eventMax,
        IntPtr hmodWinEventProc, WinEventDelegate lpfnWinEventProc,
        uint idProcess, uint idThread, uint dwFlags);

    [DllImport("user32.dll")]
    private static extern bool UnhookWinEvent(IntPtr hWinEventHook);

    [DllImport("user32.dll")]
    private static extern int GetMessage(out MSG lpMsg, IntPtr hWnd,
        uint wMsgFilterMin, uint wMsgFilterMax);

    [DllImport("user32.dll")]
    private static extern bool TranslateMessage(ref MSG lpMsg);

    [DllImport("user32.dll")]
    private static extern IntPtr DispatchMessage(ref MSG lpMsg);

    [DllImport("user32.dll")]
    private static extern bool PeekMessage(out MSG lpMsg, IntPtr hWnd,
        uint wMsgFilterMin, uint wMsgFilterMax, uint wRemoveMsg);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool PostThreadMessage(uint idThread, uint Msg,
        IntPtr wParam, IntPtr lParam);

    [DllImport("kernel32.dll")]
    private static extern uint GetCurrentThreadId();

    [DllImport("kernel32.dll")]
    private static extern bool QueryPerformanceCounter(out long lpPerformanceCount);

    [DllImport("kernel32.dll")]
    private static extern bool QueryPerformanceFrequency(out long lpFrequency);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(IntPtr hWnd, StringBuilder lpClassName, int nMaxCount);

    [DllImport("user32.dll")]
    private static extern IntPtr GetParent(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);

    [DllImport("user32.dll", EntryPoint = "IsWindowVisible")]
    private static extern bool IsWindowVisibleNative(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool IsWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern bool EnumChildWindows(IntPtr hWndParent, EnumWindowsProc lpEnumFunc, IntPtr lParam);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowText(IntPtr hWnd, StringBuilder lpString, int nMaxCount);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowTextLength(IntPtr hWnd);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool PostMessage(IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool GetLayeredWindowAttributes(IntPtr hwnd, out uint pcrKey,
        out byte pbAlpha, out uint pdwFlags);
}
