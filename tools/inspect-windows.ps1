# Which Explorer window(s) would the daemon act on right now?
# Read-only: lists cabinets with visibility / position / tab count.
Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class Insp {
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, IntPtr t);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  public static List<IntPtr> Cabinets() { var l = new List<IntPtr>(); IntPtr h = IntPtr.Zero; while ((h = FindWindowEx(IntPtr.Zero, h, "CabinetWClass", IntPtr.Zero)) != IntPtr.Zero) l.Add(h); return l; }
  public static string Title(IntPtr h) { var sb = new StringBuilder(512); GetWindowText(h, sb, 512); return sb.ToString(); }
  public static int Tabs(IntPtr h) { int n=0; IntPtr c=IntPtr.Zero; while ((c = FindWindowEx(h, c, "ShellTabWindowClass", IntPtr.Zero)) != IntPtr.Zero) n++; return n; }
}
"@
foreach ($h in [Insp]::Cabinets()) {
    $r = New-Object Insp+RECT; [void][Insp]::GetWindowRect($h, [ref]$r)
    $ex = [Insp]::GetWindowLong($h, -20)
    $layered = ($ex -band 0x80000) -ne 0
    "0x{0:x} vis={1} layered={2} rect=({3},{4})-({5},{6}) tabs={7} title=[{8}]" -f $h.ToInt64(), [Insp]::IsWindowVisible($h), $layered, $r.Left, $r.Top, $r.Right, $r.Bottom, [Insp]::Tabs($h), [Insp]::Title($h)
}
