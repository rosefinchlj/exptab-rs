param(
    [Parameter(Mandatory = $true)][string]$MainHwnd,
    [int]$Rounds = 6,
    [string]$OpenFolder = "",
    [string]$OpenNamespace = ""
)

Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class WinProbe {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string cls, string title);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  public static int Left(IntPtr h) { RECT r; GetWindowRect(h, out r); return r.Left; }
  public static int TabCount(IntPtr h) {
    int n = 0; IntPtr c = IntPtr.Zero;
    while (true) {
      c = FindWindowEx(h, c, "ShellTabWindowClass", null);
      if (c == IntPtr.Zero) break;
      n++;
    }
    return n;
  }
  public static string Title(IntPtr h) { var sb = new StringBuilder(512); GetWindowText(h, sb, 512); return sb.ToString(); }
  public static List<IntPtr> Cabinets() {
    var list = new List<IntPtr>();
    EnumWindows((h, l) => {
      var sb = new StringBuilder(64);
      GetClassName(h, sb, 64);
      if (sb.ToString() == "CabinetWClass") list.Add(h);
      return true;
    }, IntPtr.Zero);
    return list;
  }
}
"@

$main = [IntPtr][Convert]::ToInt64($MainHwnd, 16)
"main=0x{0:x}" -f $main.ToInt64()
$global:maxTabs = [WinProbe]::TabCount($main)
$global:changes = @()
$global:visibleCabinets = @()

for ($i = 0; $i -lt $Rounds; $i++) {
    $before = [WinProbe]::TabCount($main)
    $t0 = Get-Date
    if ($OpenFolder -ne "") {
        Start-Process explorer.exe -ArgumentList "/n,$OpenFolder" | Out-Null
    } elseif ($OpenNamespace -ne "") {
        Start-Process explorer.exe -ArgumentList "/n,$OpenNamespace" | Out-Null
    } else {
        Start-Process explorer.exe | Out-Null
    }
    $end = $t0.AddMilliseconds(3000)
    while ((Get-Date) -lt $end) {
        $tabs = [WinProbe]::TabCount($main)
        if ($tabs -gt $global:maxTabs) { $global:maxTabs = $tabs }
        if ($tabs -ne $before) {
            $global:changes += ("round {0} +{1}ms tabs={2} title={3}" -f $i, [int]((Get-Date) - $t0).TotalMilliseconds, $tabs, [WinProbe]::Title($main))
        }
        $vis = @([WinProbe]::Cabinets() | Where-Object { [WinProbe]::IsWindowVisible($_) -and [WinProbe]::Left($_) -gt -30000 })
        if ($vis.Count -gt 1) {
            $global:visibleCabinets += ("round {0} +{1}ms visible={2}" -f $i, [int]((Get-Date) - $t0).TotalMilliseconds, $vis.Count)
        }
        Start-Sleep -Milliseconds 40
    }
    $after = [WinProbe]::TabCount($main)
    "round {0}: before={1} after={2} max={3} title=[{4}]" -f $i, $before, $after, $global:maxTabs, [WinProbe]::Title($main)
    Start-Sleep -Milliseconds 1200
}

"max tabs seen on main     : $($global:maxTabs)"
"tab-count changes         : $($global:changes.Count)"
$global:changes | Select-Object -First 8
"extra visible cabinets    : $($global:visibleCabinets.Count)"
$global:visibleCabinets | Select-Object -First 8
