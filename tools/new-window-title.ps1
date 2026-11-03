param()

Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class NewWinProbe {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string cls, string title);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  public static int TabCount(IntPtr h) {
    int n = 0; IntPtr c = IntPtr.Zero;
    while (true) { c = FindWindowEx(h, c, "ShellTabWindowClass", null); if (c == IntPtr.Zero) break; n++; }
    return n;
  }
  public static string Title(IntPtr h) { var sb = new StringBuilder(512); GetWindowText(h, sb, 512); return sb.ToString(); }
  public static string ClassName(IntPtr h) { var sb = new StringBuilder(128); GetClassName(h, sb, 128); return sb.ToString(); }
  public static int Left(IntPtr h) { RECT r; GetWindowRect(h, out r); return r.Left; }
  public static void Close(IntPtr h) { PostMessage(h, 0x0010, IntPtr.Zero, IntPtr.Zero); }
  public static List<IntPtr> Cabinets() {
    var list = new List<IntPtr>();
    EnumWindows((h, l) => { if (ClassName(h) == "CabinetWClass") list.Add(h); return true; }, IntPtr.Zero);
    return list;
  }
}
"@

$before = @{}
foreach ($h in [NewWinProbe]::Cabinets()) { $before[$h.ToInt64()] = $true }
"existing windows: $($before.Count)"

$t0 = Get-Date
Start-Process explorer.exe | Out-Null
$seen = @{}
$end = $t0.AddMilliseconds(2800)
while ((Get-Date) -lt $end) {
    foreach ($h in [NewWinProbe]::Cabinets()) {
        $id = $h.ToInt64()
        if ($before.ContainsKey($id)) { continue }
        if (-not $seen.ContainsKey($id)) { $seen[$id] = New-Object System.Collections.Generic.List[string] }
        $line = "{0,5}ms title=[{1}] tabs={2} vis={3} left={4}" -f [int]((Get-Date) - $t0).TotalMilliseconds, [NewWinProbe]::Title($h), [NewWinProbe]::TabCount($h), [NewWinProbe]::IsWindowVisible($h), [NewWinProbe]::Left($h)
        $seen[$id].Add($line)
    }
    Start-Sleep -Milliseconds 25
}

foreach ($id in $seen.Keys) {
    $hwnd = [IntPtr]$id
    "=== window 0x{0:x}" -f $id
    $prev = ""
    foreach ($line in $seen[$id]) {
        $key = ($line -split "title=| tabs=| vis=| left=")[1]
        if ($key -ne $prev) { $line; $prev = $key }
    }
    [NewWinProbe]::Close($hwnd)
}
Start-Sleep -Milliseconds 1500
foreach ($h in [NewWinProbe]::Cabinets()) {
    if (-not $before.ContainsKey($h.ToInt64())) { [NewWinProbe]::Close($h) }
}
