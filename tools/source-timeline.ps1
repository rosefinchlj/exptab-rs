param(
    [Parameter(Mandatory = $true)][string]$MainHwnd,
    [int]$Rounds = 3
)

Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class TlProbe {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string cls, string title);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  public static int TabCount(IntPtr h) {
    int n = 0; IntPtr c = IntPtr.Zero;
    while (true) { c = FindWindowEx(h, c, "ShellTabWindowClass", null); if (c == IntPtr.Zero) break; n++; }
    return n;
  }
  public static string Title(IntPtr h) { var sb = new StringBuilder(512); GetWindowText(h, sb, 512); return sb.ToString(); }
  public static string ClassName(IntPtr h) { var sb = new StringBuilder(128); GetClassName(h, sb, 128); return sb.ToString(); }
  public static int Left(IntPtr h) { RECT r; GetWindowRect(h, out r); return r.Left; }
  public static List<IntPtr> Cabinets() {
    var list = new List<IntPtr>();
    EnumWindows((h, l) => { if (ClassName(h) == "CabinetWClass") list.Add(h); return true; }, IntPtr.Zero);
    return list;
  }
}
"@

$main = [IntPtr][Convert]::ToInt64($MainHwnd, 16)
"main=0x{0:x} tabs={1}" -f $main.ToInt64(), [TlProbe]::TabCount($main)

for ($i = 0; $i -lt $Rounds; $i++) {
    $t0 = Get-Date
    Start-Process explorer.exe | Out-Null
    $seen = @{}
    $end = $t0.AddMilliseconds(2600)
    while ((Get-Date) -lt $end) {
        foreach ($h in [TlProbe]::Cabinets()) {
            if ($h -eq $main) { continue }
            $key = $h.ToInt64().ToString("x")
            $title = [TlProbe]::Title($h)
            $tabs = [TlProbe]::TabCount($h)
            $shown = [TlProbe]::IsWindowVisible($h)
            $left = [TlProbe]::Left($h)
            $ms = [int]((Get-Date) - $t0).TotalMilliseconds
            if (-not $seen.ContainsKey($key)) { $seen[$key] = @{ hwnd = $key; t = $ms } }
            $e = $seen[$key]
            if ($title -ne "" -and -not $e.ContainsKey("title_ms")) { $e["title_ms"] = $ms; $e["title"] = $title }
            if ($tabs -gt 0 -and -not $e.ContainsKey("tabs_ms")) { $e["tabs_ms"] = $ms }
            if ($shown -and -not $e.ContainsKey("vis_ms")) { $e["vis_ms"] = $ms }
            if ($left -gt -30000 -and -not $e.ContainsKey("onscreen_ms")) { $e["onscreen_ms"] = $ms }
            $e["last"] = $ms
        }
        Start-Sleep -Milliseconds 15
    }
    foreach ($k in $seen.Keys) {
        $e = $seen[$k]
        "round {0} hwnd=0x{1} first={2}ms title@{3}ms tabs@{4}ms visible@{5}ms onscreen@{6}ms last={7}ms title=[{8}]" -f `
            $i, $e.hwnd, $e.t, $e["title_ms"], $e["tabs_ms"], $e["vis_ms"], $e["onscreen_ms"], $e["last"], $e["title"]
    }
    Start-Sleep -Milliseconds 2500
}
