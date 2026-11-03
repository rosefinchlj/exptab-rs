# bench-merge.ps1 - external, tool-agnostic measurement of "new Explorer window merges into main window".
# Low-overhead polling: only cheap Win32 queries in the hot loop (FindWindowEx/IsWindow/IsWindowVisible/
# GetLayeredWindowAttributes/GetWindowRect); titles are read at ~every 100ms because GetWindowTextW is a
# blocking cross-process WM_GETTEXT and would otherwise stall Explorer's UI thread and distort the numbers.
#
# Usage:
#   pwsh tools\bench-merge.ps1 -Runs 10 -MainHwnd 0x82636 -Label rust   -CloseTabs
#   pwsh tools\bench-merge.ps1 -Runs 10 -MainHwnd 0xE019A8 -Label csharp -CloseTabs

param(
  [int]$Runs = 10,
  [string]$MainHwnd = "",
  [string]$Label = "run",
  [string]$Base = "",
  [int]$TimeoutMs = 9000,
  [int]$PollMs = 2,
  [switch]$CloseTabs
)

$ErrorActionPreference = 'Stop'
$dir = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $Base) { $Base = Join-Path $env:TEMP "exptab-bench-merge-$Label" }
New-Item -ItemType Directory -Force -Path $Base | Out-Null
$outCsv = Join-Path $dir ("bench-out\merge-$Label-" + (Get-Date -Format 'yyyyMMdd-HHmmss') + ".csv")
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $outCsv) | Out-Null

Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class M {
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr after, string cls, IntPtr title);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder sb, int n);
  [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetLayeredWindowAttributes(IntPtr h, out uint key, out byte alpha, out uint flags);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint msg, IntPtr wp, IntPtr lp);
  [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint msg, IntPtr wp, IntPtr lp);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
'@

function Get-Cabinets {
  $list = New-Object System.Collections.Generic.List[IntPtr]
  $after = [IntPtr]::Zero
  while ($true) {
    $h = [M]::FindWindowExW([IntPtr]::Zero, $after, "CabinetWClass", [IntPtr]::Zero)
    if ($h -eq [IntPtr]::Zero) { break }
    $list.Add($h); $after = $h
  }
  return $list
}
function Get-Tabs([IntPtr]$w) {
  $list = New-Object System.Collections.Generic.List[IntPtr]
  $after = [IntPtr]::Zero
  while ($true) {
    $h = [M]::FindWindowExW($w, $after, "ShellTabWindowClass", [IntPtr]::Zero)
    if ($h -eq [IntPtr]::Zero) { break }
    $list.Add($h); $after = $h
  }
  return $list
}
function Get-Text([IntPtr]$h) { $sb = New-Object System.Text.StringBuilder 512; [void][M]::GetWindowTextW($h, $sb, 512); $sb.ToString() }
function Get-Alpha([IntPtr]$h) { $k = 0; $a = [byte]255; $f = 0; if ([M]::GetLayeredWindowAttributes($h, [ref]$k, [ref]$a, [ref]$f)) { return $a } else { return -1 } }

$main = if ($MainHwnd) { [IntPtr][Convert]::ToInt64($MainHwnd, 16) } else { (Get-Cabinets)[0] }
if (-not [M]::IsWindow($main)) { throw "main window $MainHwnd not found" }
$mainTabs0 = @(Get-Tabs $main).Count
Write-Host ("bench[{0}] main=0x{1:X} tabs={2} runs={3}" -f $Label, $main.ToInt64(), $mainTabs0, $Runs)

$rows = @()
for ($i = 1; $i -le $Runs; $i++) {
  $leaf = "{0}-{1:00}" -f $Label, $i
  $sub = Join-Path $Base $leaf
  New-Item -ItemType Directory -Force -Path $sub | Out-Null
  $known = @{}
  foreach ($c in Get-Cabinets) { $known[$c.ToInt64()] = $true }
  $beforeTabs = @{}
  foreach ($t in Get-Tabs $main) { $beforeTabs[$t.ToInt64()] = $true }
  $tabsBefore = $beforeTabs.Count

  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  Start-Process explorer.exe -ArgumentList "/n,`"$sub`"" | Out-Null

  $src = [IntPtr]::Zero; $tDetect = -1; $tHide = -1; $tTab = -1; $tClose = -1
  $tTabSeen = -1; $matched = $false; $tVis = -1; $visSeen = $false; $tabTitle = ""
  $nextTitleCheck = 0
  while ($sw.ElapsedMilliseconds -lt $TimeoutMs) {
    if ($tDetect -lt 0) {
      foreach ($c in Get-Cabinets) {
        $id = $c.ToInt64()
        if (-not $known.ContainsKey($id)) { $known[$id] = $true; $src = $c; $tDetect = $sw.ElapsedMilliseconds; break }
      }
    }
    if ($src -ne [IntPtr]::Zero) {
      if ($tHide -lt 0 -and (Get-Alpha $src) -eq 0) { $tHide = $sw.ElapsedMilliseconds }
      if ($tVis -lt 0 -and [M]::IsWindow($src) -and [M]::IsWindowVisible($src)) { $tVis = $sw.ElapsedMilliseconds; $visSeen = $true }
      $tabs = @(Get-Tabs $main)
      if ($tabs.Count -gt $tabsBefore) {
        if ($tTabSeen -lt 0) { $tTabSeen = $sw.ElapsedMilliseconds }
        $newOnes = @($tabs | Where-Object { -not $beforeTabs.ContainsKey($_.ToInt64()) })
        if ($sw.ElapsedMilliseconds -ge $nextTitleCheck) {
          $nextTitleCheck = $sw.ElapsedMilliseconds + 100
          foreach ($t in $newOnes) {
            $tt = Get-Text $t
            if ($tt) { $tabTitle = $tt }
            if ($tt -like "*$leaf*") { $tTab = $tTabSeen; $matched = $true }
          }
        }
      }
      if ($tClose -lt 0 -and -not [M]::IsWindow($src)) { $tClose = $sw.ElapsedMilliseconds }
      if ($matched -and $tClose -ge 0) { break }
    }
    Start-Sleep -Milliseconds $PollMs
  }
  if ($tTab -lt 0 -and $tTabSeen -ge 0) { $tTab = $tTabSeen }
  $sw.Stop()

  $flash = if ($visSeen -and $tHide -ge 0) { [math]::Max(0, $tHide - $tVis) } elseif ($visSeen) { 9999 } else { 0 }
  $merged = ($matched -and $tClose -ge 0)
  $row = [pscustomobject]@{
    run = $i; label = $Label; leaf = $leaf; merged = $merged
    spawn_to_detect_ms = $tDetect; spawn_to_hide_ms = $tHide; spawn_to_tab_ms = $tTab; spawn_to_close_ms = $tClose
    detect_to_hide_ms = if ($tDetect -ge 0 -and $tHide -ge 0) { $tHide - $tDetect } else { -1 }
    tab_title = $tabTitle; flashed_ms = $flash; src_hwnd = ('0x{0:X}' -f $src.ToInt64())
    main = ('0x{0:X}' -f $main.ToInt64()); main_tabs_before = $tabsBefore; main_tabs_after = @(Get-Tabs $main).Count
  }
  $rows += $row
  "{0,2}/{1}: merged={2,-5} detect={3,4} hide={4,4} tab={5,4} close={6,4} detect->hide={7,4} flash={8,4} title='{9}'" -f `
    $i, $Runs, $merged, $tDetect, $tHide, $tTab, $tClose, $row.detect_to_hide_ms, $flash, $tabTitle | Write-Host

  # cleanup: close tabs added to the main window by this run, then any leftover source window
  if ($CloseTabs) {
    foreach ($t in @(Get-Tabs $main)) {
      if (-not $beforeTabs.ContainsKey($t.ToInt64())) { [void][M]::SendMessageW($t, 0x111, [IntPtr]0xA021, [IntPtr]1) }
    }
  }
  if ($src -ne [IntPtr]::Zero -and [M]::IsWindow($src)) {
    $tabs = @(Get-Tabs $src)
    if ($tabs.Count -gt 0) { [void][M]::SendMessageW($tabs[0], 0x111, [IntPtr]0xA021, [IntPtr]0) }
    Start-Sleep -Milliseconds 400
    if ([M]::IsWindow($src)) { [void][M]::PostMessageW($src, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) }
  }
  Start-Sleep -Milliseconds 250
}

$rows | Export-Csv -Path $outCsv -NoTypeInformation -Encoding UTF8
function Stats($vals) {
  $v = @($vals | Where-Object { $_ -ge 0 } | Sort-Object)
  if ($v.Count -eq 0) { return @{ n = 0 } }
  $p = { param($q) $v[[math]::Min($v.Count - 1, [math]::Floor($q * ($v.Count - 1)))] }
  return @{ n = $v.Count; min = $v[0]; p50 = (& $p 0.5); p95 = (& $p 0.95); max = $v[$v.Count - 1]; mean = [math]::Round(($v | Measure-Object -Average).Average, 1) }
}
$summary = [pscustomobject]@{
  label = $Label; runs = $Runs; merged_ok = @($rows | Where-Object { $_.merged }).Count
  spawn_to_detect = (Stats ($rows.spawn_to_detect_ms)) | ConvertTo-Json -Compress
  spawn_to_hide   = (Stats ($rows.spawn_to_hide_ms))   | ConvertTo-Json -Compress
  spawn_to_tab    = (Stats ($rows.spawn_to_tab_ms))    | ConvertTo-Json -Compress
  spawn_to_close  = (Stats ($rows.spawn_to_close_ms))  | ConvertTo-Json -Compress
  detect_to_hide  = (Stats ($rows.detect_to_hide_ms))  | ConvertTo-Json -Compress
  flashed         = (Stats ($rows.flashed_ms))         | ConvertTo-Json -Compress
}
$summary | Format-List
$summary | ConvertTo-Json -Depth 4 | Set-Content -Path ($outCsv -replace '\.csv$', '-summary.json')
Write-Host "csv: $outCsv"
