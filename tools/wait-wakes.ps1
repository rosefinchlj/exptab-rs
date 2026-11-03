# Count how often the daemon's wait primitives actually wake.
# Usage: tools\wait-wakes.ps1 [-Seconds 3]
param([int]$Seconds = 3)
$src = @'
using System;
using System.Runtime.InteropServices;
public static class W {
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern IntPtr CreateWaitableTimerW(IntPtr a, bool manual, IntPtr name);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool SetWaitableTimer(IntPtr h, ref long due, int period, IntPtr r, IntPtr ctx, bool resume);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern IntPtr CreateEventW(IntPtr a, bool manual, bool init, IntPtr name);
    [DllImport("kernel32.dll")]
    static extern bool SetEvent(IntPtr h);
    [DllImport("kernel32.dll")]
    static extern bool ResetEvent(IntPtr h);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern uint WaitForSingleObject(IntPtr h, uint ms);
    [DllImport("user32.dll")]
    static extern IntPtr SetWinEventHook(uint min, uint max, IntPtr mod, HookProc cb, uint pid, uint tid, uint flags);
    delegate void HookProc(IntPtr hook, uint evt, IntPtr hwnd, int idObj, int idChild, uint tid, uint time);
    static IntPtr ev;
    static HookProc keep;
    public static string Run(int secs) {
        ev = CreateEventW(IntPtr.Zero, true, false, IntPtr.Zero);
        var timer = CreateWaitableTimerW(IntPtr.Zero, false, IntPtr.Zero);
        keep = new HookProc((hook, evt, hwnd, o, c, t, ts) => { SetEvent(ev); });
        SetWinEventHook(0x8000, 0x8001, IntPtr.Zero, keep, 0, 0, 0);
        var sw = System.Diagnostics.Stopwatch.StartNew();
        long tw = 0, ew = 0;
        while (sw.Elapsed.TotalSeconds < secs) {
            ResetEvent(ev);
            long due = -100000; // 10 ms
            SetWaitableTimer(timer, ref due, 0, IntPtr.Zero, IntPtr.Zero, false);
            uint r = WaitForSingleObject(ev, 10);
            if (r == 258) { tw++; } else { ew++; }
        }
        long ms = sw.ElapsedMilliseconds;
        return string.Format("wall={0}ms timer-wakes={1} event-wakes={2} total={3} -> {4:N1} wakes/s",
            ms, tw, ew, tw + ew, (tw + ew) / (ms / 1000.0));
    }
}
'@
Add-Type -TypeDefinition $src -Language CSharp
[W]::Run($Seconds)
