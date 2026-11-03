#requires -Version 7.0
<#
.SYNOPSIS
    Measures the "new Explorer window -> tab" latency of ExplorerTabUtility (or a
    replacement) using a global WinEvent hook.

.DESCRIPTION
    For every run:
      1. snapshot the existing CabinetWClass windows and their tabs (baseline),
      2. install the WinEvent hook,
      3. start  explorer.exe /n,<path>  and timestamp the launch with QPC,
      4. poll for up to -TimeoutMs for: the new window's create/show, its
         hide (alpha=0 or destroy) and a new ShellTabWindowClass under one of
         the baseline windows,
      5. record one CSV row and close only the window this run created.

    The script never closes pre-existing windows and never closes tabs, even
    when a merge left a new tab behind in the main window.

.PARAMETER Runs
    Number of measured runs (default 10).  Use 1 for a smoke test.

.PARAMETER Path
    Directory opened by each run when -UniqueSubdir is disabled.  Ignored when
    -UniqueSubdir is on (the default), which opens a freshly created directory
    under -ProbeRoot instead.

.PARAMETER OutCsv
    CSV output path.  Defaults to tools\bench-out\run-<timestamp>.csv.

.PARAMETER DryRun
    Only installs the hook and records a short mark-only timeline; does not
    start Explorer and does not touch any window.

.PARAMETER UniqueSubdir
    Default $true.  Each run opens a brand new directory under -ProbeRoot.
    This defeats ExplorerTabUtility's ReuseTabs behaviour, which would
    otherwise select an existing tab for an already-open path and never create
    a new ShellTabWindowClass.  Disable with -UniqueSubdir:$false to measure a
    fixed -Path.

.PARAMETER ProbeRoot
    Root for per-run probe directories (default %TEMP%\exptab-bench-probe).

.PARAMETER TimeoutMs
    Per-run decision deadline (default 5000).

.PARAMETER PollMs
    Poll interval (default 20).

.PARAMETER GraceMs
    Extra time to wait for the source window's destroy event after the merge
    decision has been made (default 1500).  The C# tool destroys the source
    window via a COM Quit, which can lag the new tab by several hundred ms.

.PARAMETER DumpTimeline
    Also write a per-run timeline CSV next to OutCsv and print the first run's
    timeline to the console.

.EXAMPLE
    .\bench.ps1 -DryRun

.EXAMPLE
    .\bench.ps1 -Runs 1 -DumpTimeline

.EXAMPLE
    .\bench.ps1 -Runs 10 -Path 'C:\Windows\System32' -UniqueSubdir:$false
#>
[CmdletBinding()]
param(
    [int]$Runs = 10,
    [string]$Path = 'C:\Windows\System32',
    [string]$OutCsv,
    [switch]$DryRun,
    [bool]$UniqueSubdir = $true,
    [string]$ProbeRoot = (Join-Path ([System.IO.Path]::GetTempPath()) 'exptab-bench-probe'),
    [int]$TimeoutMs = 5000,
    [int]$PollMs = 20,
    [int]$GraceMs = 1500,
    [switch]$DumpTimeline
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$script:CsPath = Join-Path $PSScriptRoot 'WinEventTimeline.cs'
$script:ToolProcessName = 'ExplorerTabUtility'

function Write-Step { param([string]$Message) Write-Host $Message }
function Write-Note { param([string]$Message) Write-Host $Message -ForegroundColor DarkGray }
function Write-Warn { param([string]$Message) Write-Host "WARN: $Message" -ForegroundColor Yellow }

# ---------------------------------------------------------------------------
# C# timeline helper
# ---------------------------------------------------------------------------

function Initialize-TimelineType {
    if ('WinEventTimeline' -as [type]) { return }
    if (-not (Test-Path -LiteralPath $script:CsPath)) {
        throw "Missing C# source file: $($script:CsPath)"
    }
    $code = Get-Content -LiteralPath $script:CsPath -Raw
    Add-Type -TypeDefinition $code -Language CSharp
    if (-not ('WinEventTimeline' -as [type])) {
        throw 'Add-Type completed but type WinEventTimeline is unavailable.'
    }
}

# ---------------------------------------------------------------------------
# State helpers
# ---------------------------------------------------------------------------

function Get-ExplorerState {
    $windows = [WinEventTimeline]::GetExplorerWindows()
    $hwnds = [System.Collections.Generic.HashSet[long]]::new()
    $tabs = [System.Collections.Generic.HashSet[long]]::new()
    foreach ($w in $windows) {
        [void]$hwnds.Add([long]$w.hwnd)
        foreach ($t in $w.tabs) { [void]$tabs.Add([long]$t) }
    }
    [pscustomobject]@{
        Windows = $windows
        Hwnds   = $hwnds
        Tabs    = $tabs
    }
}

<#
    Extracts the run's timeline facts from an event snapshot plus the polling
    observations taken by Invoke-BenchRun.

    "Source" is the earliest-observed new CabinetWClass window.  Its creation
    time is the earliest of (EVENT_OBJECT_CREATE, first EnumWindows sighting):
    Windows sometimes delivers create events for Explorer windows 0.3-2 s late,
    so the poll sighting is often the better clock.  "Merge tab" is the
    earliest-observed new ShellTabWindowClass whose parent is a baseline window.
#>
function Get-RunState {
    param(
        $Events,
        $Baseline,
        [hashtable]$FirstSeen = $null,
        [hashtable]$FirstVisible = $null,
        [hashtable]$FirstHidden = $null,
        [hashtable]$FirstHiddenWhy = $null,
        [hashtable]$FirstGone = $null,
        [hashtable]$FirstTab = $null
    )

    $allNew = [System.Collections.Generic.HashSet[long]]::new()
    $createEventUs = @{}
    $tabEventUs = @{}
    $tabEventParent = @{}
    $showEventUs = -1.0
    $hideEventUs = -1.0
    $destroyEventUs = -1.0

    foreach ($e in @($Events)) {
        $h = [long]$e.hwnd
        if ($e.cls -eq 'CabinetWClass' -and $e.evt -eq 'create' -and -not $Baseline.Hwnds.Contains($h)) {
            [void]$allNew.Add($h)
            if (-not $createEventUs.ContainsKey($h)) { $createEventUs[$h] = [double]$e.t_us }
        }
        if ($e.evt -eq 'create' -and $e.cls -eq 'ShellTabWindowClass' -and $h -ne 0) {
            $p = [long]$e.parent
            if ($p -ne 0 -and $Baseline.Hwnds.Contains($p) -and -not $Baseline.Tabs.Contains($h) -and
                -not $tabEventUs.ContainsKey($h)) {
                $tabEventUs[$h] = [double]$e.t_us
                $tabEventParent[$h] = $p
            }
        }
    }
    if ($null -ne $FirstSeen) {
        foreach ($k in $FirstSeen.Keys) {
            if (-not $Baseline.Hwnds.Contains([long]$k)) { [void]$allNew.Add([long]$k) }
        }
    }

    # Source = earliest new CabinetWClass by (create event, first poll sighting).
    $sourceHwnd = [long]0
    $sourcePid = 0
    $createUs = -1.0
    $createSource = ''
    $bestUs = [double]::MaxValue
    foreach ($h in $allNew) {
        $u = [double]::MaxValue
        $src = ''
        if ($createEventUs.ContainsKey($h)) { $u = [double]$createEventUs[$h]; $src = 'event' }
        if ($null -ne $FirstSeen -and $FirstSeen.ContainsKey($h)) {
            $pu = [double]$FirstSeen[$h]
            if ($pu -lt $u) { $u = $pu; $src = 'poll' }
        }
        if ($u -lt $bestUs) { $bestUs = $u; $sourceHwnd = $h; $createSource = $src }
    }
    if ($sourceHwnd -ne 0) { $createUs = $bestUs }

    foreach ($e in @($Events)) {
        if ([long]$e.hwnd -eq $sourceHwnd -and $e.pid -ne 0) { $sourcePid = [int]$e.pid; break }
    }
    if ($sourceHwnd -ne 0) {
        foreach ($e in @($Events)) {
            if ([long]$e.hwnd -ne $sourceHwnd -or $e.cls -ne 'CabinetWClass') { continue }
            if ($e.evt -eq 'show' -and $showEventUs -lt 0) { $showEventUs = [double]$e.t_us }
            elseif ($e.evt -eq 'hide' -and $hideEventUs -lt 0) { $hideEventUs = [double]$e.t_us }
            elseif ($e.evt -eq 'destroy' -and $destroyEventUs -lt 0) { $destroyEventUs = [double]$e.t_us }
        }
    }

    # Merge tab = earliest new ShellTabWindowClass whose parent is a baseline window.
    $tabCandidates = [System.Collections.Generic.HashSet[long]]::new()
    foreach ($k in $tabEventUs.Keys) { [void]$tabCandidates.Add([long]$k) }
    if ($null -ne $FirstTab) {
        foreach ($k in $FirstTab.Keys) { [void]$tabCandidates.Add([long]$k) }
    }
    $tabHwnd = [long]0
    $tabParent = [long]0
    $tabUs = -1.0
    $tabSource = ''
    $bestTabUs = [double]::MaxValue
    foreach ($h in $tabCandidates) {
        if ($Baseline.Tabs.Contains($h)) { continue }
        $u = [double]::MaxValue
        $p = [long]0
        $src = ''
        if ($tabEventUs.ContainsKey($h)) { $u = [double]$tabEventUs[$h]; $p = [long]$tabEventParent[$h]; $src = 'event' }
        if ($null -ne $FirstTab -and $FirstTab.ContainsKey($h)) {
            $rec = $FirstTab[$h]
            $pu = [double]$rec.us
            if ($pu -lt $u) { $u = $pu; $p = [long]$rec.parent; $src = 'poll' }
        }
        if ($p -eq 0 -or -not $Baseline.Hwnds.Contains($p)) { continue }
        if ($u -lt $bestTabUs) {
            $bestTabUs = $u
            $tabHwnd = $h
            $tabParent = $p
            $tabUs = $u
            $tabSource = $src
        }
    }

    $showUs = $showEventUs
    if ($null -ne $FirstVisible -and $sourceHwnd -ne 0 -and $FirstVisible.ContainsKey($sourceHwnd)) {
        $pu = [double]$FirstVisible[$sourceHwnd]
        if ($showUs -lt 0 -or $pu -lt $showUs) { $showUs = $pu }
    }

    $hideUs = $hideEventUs
    $hideSource = ''
    if ($hideUs -ge 0) { $hideSource = 'event' }
    if ($null -ne $FirstHidden -and $sourceHwnd -ne 0 -and $FirstHidden.ContainsKey($sourceHwnd)) {
        $pu = [double]$FirstHidden[$sourceHwnd]
        if ($hideUs -lt 0 -or $pu -lt $hideUs) {
            $hideUs = $pu
            $hideSource = 'poll'
            if ($null -ne $FirstHiddenWhy -and $FirstHiddenWhy.ContainsKey($sourceHwnd)) {
                $hideSource = 'poll-' + [string]$FirstHiddenWhy[$sourceHwnd]
            }
        }
    }

    $destroyUs = $destroyEventUs
    if ($null -ne $FirstGone -and $sourceHwnd -ne 0 -and $FirstGone.ContainsKey($sourceHwnd)) {
        $pu = [double]$FirstGone[$sourceHwnd]
        if ($destroyUs -lt 0 -or $pu -lt $destroyUs) { $destroyUs = $pu }
    }

    [pscustomobject]@{
        SourceHwnd     = $sourceHwnd
        SourcePid      = $sourcePid
        CreateUs       = $createUs
        CreateSource   = $createSource
        ShowUs         = $showUs
        HideUs         = $hideUs
        HideSource     = $hideSource
        DestroyUs      = $destroyUs
        NewTabHwnd     = $tabHwnd
        NewTabParent   = $tabParent
        NewTabUs       = $tabUs
        TabSource      = $tabSource
        CreatedWindows = @($allNew)
    }
}

function Close-TestWindows {
    param(
        $State,
        $Baseline,
        [string]$Leaf
    )

    foreach ($h in $State.CreatedWindows) {
        if ($Baseline.Hwnds.Contains($h)) { continue }
        if (-not [WinEventTimeline]::IsWindowAlive($h)) { continue }

        $info = [WinEventTimeline]::GetWindowInfo($h)
        if ($null -eq $info) { continue }

        $proc = Get-Process -Id $info.pid -ErrorAction SilentlyContinue
        if ($null -eq $proc -or $proc.ProcessName -ne 'explorer') {
            Write-Warn ("refusing to close hwnd=0x{0:X}: owning process is not explorer.exe" -f $h)
            continue
        }

        # Only close windows that look like the probe window.  A visible window
        # with an unrelated (or not yet set) title is left alone; an alpha-0
        # window can only be a merged-away test window (the user never sees it).
        $titleOk = (-not [string]::IsNullOrEmpty($Leaf)) -and ($info.title -like "*$Leaf*")
        if (-not $titleOk -and $info.layered_alpha -ne 0) {
            Write-Warn ("refusing to close hwnd=0x{0:X} title='{1}': does not look like the probe window" -f $h, $info.title)
            continue
        }

        [void][WinEventTimeline]::CloseWindow($h)
        $deadline = [datetime]::UtcNow.AddMilliseconds(2000)
        while ([datetime]::UtcNow -lt $deadline -and [WinEventTimeline]::IsWindowAlive($h)) {
            Start-Sleep -Milliseconds 25
        }
        if ([WinEventTimeline]::IsWindowAlive($h)) {
            Write-Warn ("test window 0x{0:X} is still alive after WM_CLOSE" -f $h)
        }
    }
}

# ---------------------------------------------------------------------------
# One measured run
# ---------------------------------------------------------------------------

function Invoke-BenchRun {
    param(
        [int]$Index,
        [int]$Total,
        [string]$RunPath,
        [string]$CsvBaseName,
        [string]$OutDir
    )

    $baseline = Get-ExplorerState
    if ($baseline.Windows.Count -eq 0) {
        throw 'No pre-existing CabinetWClass window found; refusing to run (cannot distinguish test windows from user windows).'
    }

    $events = $null
    $firstSeen = @{}
    $firstSeenPid = @{}
    $firstVisible = @{}
    $firstHidden = @{}
    $firstHiddenWhy = @{}
    $firstGone = @{}
    $firstTab = @{}

    [WinEventTimeline]::Start()
    try {
        # Refresh the baseline under the hook so it reflects the windows that
        # existed immediately before the spawn.
        $baseline = Get-ExplorerState
        [WinEventTimeline]::Mark('spawn')
        $spawnQpc = [WinEventTimeline]::SpawnExplorer($RunPath)
        $spawnUs = [WinEventTimeline]::UsSinceStart($spawnQpc)

        $knownNew = [System.Collections.Generic.HashSet[long]]::new()
        $state = $null

        $deadline = [datetime]::UtcNow.AddMilliseconds($TimeoutMs)
        while ($true) {
            # EnumWindows polling is the authoritative "the window exists" clock:
            # EVENT_OBJECT_CREATE for Explorer windows is sometimes delivered
            # 0.3-2 s late, so a poll sighting can legitimately precede it.
            $q0 = [WinEventTimeline]::NowQpc()
            $es = Get-ExplorerState
            $q1 = [WinEventTimeline]::NowQpc()
            $mid = [WinEventTimeline]::UsSinceStart([long](($q0 + $q1) / 2))

            foreach ($w in $es.Windows) {
                $h = [long]$w.hwnd
                if ($baseline.Hwnds.Contains($h)) {
                    foreach ($t in $w.tabs) {
                        $th = [long]$t
                        if (-not $baseline.Tabs.Contains($th) -and -not $firstTab.ContainsKey($th)) {
                            $firstTab[$th] = [pscustomobject]@{ us = $mid; parent = $h }
                        }
                    }
                    continue
                }
                if (-not $firstSeen.ContainsKey($h)) {
                    $firstSeen[$h] = $mid
                    $firstSeenPid[$h] = [int]$w.pid
                    [void]$knownNew.Add($h)
                }
                if ($w.visible) {
                    if (-not $firstVisible.ContainsKey($h)) { $firstVisible[$h] = $mid }
                }
                elseif ($firstVisible.ContainsKey($h) -and -not $firstHidden.ContainsKey($h)) {
                    $firstHidden[$h] = $mid
                    $firstHiddenWhy[$h] = 'invisible'
                }
                if ($firstVisible.ContainsKey($h) -and $w.layered_alpha -eq 0 -and
                    -not $firstHidden.ContainsKey($h)) {
                    $firstHidden[$h] = $mid
                    $firstHiddenWhy[$h] = 'alpha0'
                }
            }
            foreach ($h in $knownNew) {
                if (-not $es.Hwnds.Contains($h) -and -not $firstGone.ContainsKey($h)) {
                    $firstGone[$h] = $mid
                }
            }

            $state = Get-RunState -Events ([WinEventTimeline]::Snapshot()) -Baseline $baseline `
                -FirstSeen $firstSeen -FirstVisible $firstVisible -FirstHidden $firstHidden `
                -FirstHiddenWhy $firstHiddenWhy -FirstGone $firstGone -FirstTab $firstTab

            $hidden = ($state.HideUs -ge 0) -or ($state.DestroyUs -ge 0)
            if ($state.SourceHwnd -ne 0 -and $state.NewTabUs -ge 0 -and $hidden) { break }
            if ([datetime]::UtcNow -ge $deadline) { break }

            Start-Sleep -Milliseconds $PollMs
        }

        # Short grace period so the source window's destroy is not cut off.
        if ($state -ne $null -and $state.SourceHwnd -ne 0 -and $state.NewTabUs -ge 0 -and
            -not $firstGone.ContainsKey($state.SourceHwnd) -and
            [WinEventTimeline]::IsWindowAlive($state.SourceHwnd)) {
            $graceEnd = [datetime]::UtcNow.AddMilliseconds($GraceMs)
            while ([datetime]::UtcNow -lt $graceEnd) {
                if (-not [WinEventTimeline]::IsWindowAlive($state.SourceHwnd)) {
                    $firstGone[$state.SourceHwnd] = [WinEventTimeline]::UsSinceStart([WinEventTimeline]::NowQpc())
                    break
                }
                Start-Sleep -Milliseconds $PollMs
            }
        }
    }
    finally {
        $events = [WinEventTimeline]::StopAndDump()
    }

    $state = Get-RunState -Events $events -Baseline $baseline `
        -FirstSeen $firstSeen -FirstVisible $firstVisible -FirstHidden $firstHidden `
        -FirstHiddenWhy $firstHiddenWhy -FirstGone $firstGone -FirstTab $firstTab

    if ($state.SourceHwnd -ne 0 -and $state.SourcePid -eq 0) {
        if ($firstSeenPid.ContainsKey($state.SourceHwnd)) {
            $state.SourcePid = [int]$firstSeenPid[$state.SourceHwnd]
        } elseif ([WinEventTimeline]::IsWindowAlive($state.SourceHwnd)) {
            $info = [WinEventTimeline]::GetWindowInfo($state.SourceHwnd)
            if ($null -ne $info) { $state.SourcePid = [int]$info.pid }
        }
    }

    $effectiveHideUs = $state.HideUs
    if ($effectiveHideUs -lt 0) { $effectiveHideUs = $state.DestroyUs }
    $destroyUs = $state.DestroyUs

    $hidden = $effectiveHideUs -ge 0
    $merged = ($state.NewTabUs -ge 0) -and $hidden

    if ($state.SourceHwnd -eq 0) {
        $status = 'no-window'
    } elseif ($merged) {
        $status = 'merged'
    } elseif ($state.NewTabUs -ge 0) {
        $status = 'tab-no-hide'
    } elseif ($hidden) {
        $status = 'hidden-no-tab'
    } else {
        $status = 'timeout'
    }

    $tHide = $null
    $tTab = $null
    $tDestroy = $null
    $flashedMs = $null

    if ($state.CreateUs -ge 0) {
        if ($effectiveHideUs -ge 0) { $tHide = [math]::Round($effectiveHideUs - $state.CreateUs, 1) }
        if ($destroyUs -ge 0) { $tDestroy = [math]::Round($destroyUs - $state.CreateUs, 1) }
        if ($state.NewTabUs -ge 0) { $tTab = [math]::Round($state.NewTabUs - $state.CreateUs, 1) }

        if ($hidden) {
            $visibleStart = if ($state.ShowUs -ge 0) { $state.ShowUs } else { $state.CreateUs }
            $flashedMs = [math]::Round([math]::Max(0.0, ($effectiveHideUs - $visibleStart) / 1000.0), 3)
        }
    }

    if ($DumpTimeline -and $OutDir) {
        $tlPath = Join-Path $OutDir ('{0}-run{1:D2}-timeline.csv' -f $CsvBaseName, $Index)
        [System.IO.File]::WriteAllText($tlPath, [WinEventTimeline]::DumpText($events), [System.Text.UTF8Encoding]::new($false))
        if ($Index -eq 1) {
            Write-Step '--- timeline (run 1) ---'
            $lines = [WinEventTimeline]::DumpText($events) -split "`r?`n"
            $lines | Select-Object -First 45 | ForEach-Object { Write-Host "  $_" }
            Write-Step '--- end timeline ---'
        }
    }

    $leaf = ''
    try { $leaf = Split-Path -Leaf $RunPath } catch { $leaf = '' }

    $row = [pscustomobject]@{
        run                      = $Index
        target                   = $RunPath
        status                   = $status
        succeeded                = $merged
        source_hwnd              = ('0x{0:X}' -f $state.SourceHwnd)
        source_pid               = $state.SourcePid
        tab_hwnd                 = ('0x{0:X}' -f $state.NewTabHwnd)
        tab_parent               = ('0x{0:X}' -f $state.NewTabParent)
        spawn_us                 = [math]::Round($spawnUs, 1)
        create_us                = if ($state.CreateUs -ge 0) { [math]::Round($state.CreateUs, 1) } else { $null }
        create_source            = $state.CreateSource
        show_us                  = if ($state.ShowUs -ge 0) { [math]::Round($state.ShowUs, 1) } else { $null }
        hide_us                  = if ($effectiveHideUs -ge 0) { [math]::Round($effectiveHideUs, 1) } else { $null }
        hide_source              = if ($state.HideUs -ge 0) { $state.HideSource } elseif ($effectiveHideUs -ge 0) { 'destroy' } else { '' }
        destroy_us               = if ($destroyUs -ge 0) { [math]::Round($destroyUs, 1) } else { $null }
        t_create_to_hide_us      = $tHide
        t_create_to_new_tab_us   = $tTab
        t_create_to_destroy_us   = $tDestroy
        flashed_ms               = $flashedMs
        windows_before           = $baseline.Windows.Count
        tabs_before              = $baseline.Tabs.Count
        tab_source               = $state.TabSource
        note                     = if ($merged) { '' } else { 'no merge within timeout or no new tab' }
    }

    $msHide = if ($null -ne $tHide) { '{0,8:F1}' -f ($tHide / 1000.0) } else { '       -' }
    $msTab = if ($null -ne $tTab) { '{0,8:F1}' -f ($tTab / 1000.0) } else { '       -' }
    $msDestroy = if ($null -ne $tDestroy) { '{0,8:F1}' -f ($tDestroy / 1000.0) } else { '       -' }
    $msFlash = if ($null -ne $flashedMs) { '{0,8:F1}' -f $flashedMs } else { '       -' }
    Write-Step ("[{0,2}/{1}] {2,-13} hide={3}ms tab={4}ms destroy={5}ms flash={6}ms  src=0x{7:X}" -f `
        $Index, $Total, $status, $msHide, $msTab, $msDestroy, $msFlash, $state.SourceHwnd)

    Close-TestWindows -State $state -Baseline $baseline -Leaf $leaf
    return $row
}

# ---------------------------------------------------------------------------
# Statistics
# ---------------------------------------------------------------------------

function Get-Percentile {
    param(
        [double[]]$Values,
        [double]$Percent
    )
    if ($null -eq $Values -or $Values.Count -eq 0) { return $null }
    $sorted = @($Values | Sort-Object)
    $rank = [int][math]::Ceiling(($Percent / 100.0) * $sorted.Count)
    if ($rank -lt 1) { $rank = 1 }
    if ($rank -gt $sorted.Count) { $rank = $sorted.Count }
    return $sorted[$rank - 1]
}

function Show-Summary {
    param($Rows)

    $all = @($Rows)
    $merged = @($all | Where-Object { $_.status -eq 'merged' })

    Write-Step ''
    Write-Step ("=== summary: {0} runs, {1} merged, success {2:F1}% ===" -f `
        $all.Count, $merged.Count, (100.0 * $merged.Count / [math]::Max(1, $all.Count)))

    $metrics = @(
        @{ Name = 't_create_to_hide_us';    Label = 'create -> hide'; },
        @{ Name = 't_create_to_new_tab_us'; Label = 'create -> new tab'; },
        @{ Name = 't_create_to_destroy_us'; Label = 'create -> destroy'; },
        @{ Name = 'flashed_ms';             Label = 'visible (flash)'; }
    )

    Write-Step ("{0,-26} {1,10} {2,10} {3,10} {4,6}" -f 'metric', 'p50', 'p95', 'mean', 'n')
    Write-Step ('-' * 68)

    foreach ($m in $metrics) {
        $values = @($merged | ForEach-Object { $_.($m.Name) } |
            Where-Object { $null -ne $_ } | ForEach-Object { [double]$_ })
        if ($values.Count -eq 0) {
            Write-Step ("{0,-26} {1,10} {2,10} {3,10} {4,6}" -f $m.Label, '-', '-', '-', 0)
            continue
        }
        $p50 = Get-Percentile -Values $values -Percent 50
        $p95 = Get-Percentile -Values $values -Percent 95
        $mean = ($values | Measure-Object -Average).Average
        if ($m.Name -eq 'flashed_ms') {
            Write-Step ("{0,-26} {1,10:F2} {2,10:F2} {3,10:F2} {4,6}" -f $m.Label, $p50, $p95, $mean, $values.Count)
        } else {
            Write-Step ("{0,-26} {1,10:F1} {2,10:F1} {3,10:F1} {4,6}" -f $m.Label, ($p50 / 1000.0), ($p95 / 1000.0), ($mean / 1000.0), $values.Count)
        }
    }
    Write-Step ''
    Write-Step 'Units: create->hide / new tab / destroy in ms, flash in ms; raw CSV keeps microseconds and ms.'
}

# ---------------------------------------------------------------------------
# Dry run
# ---------------------------------------------------------------------------

function Invoke-DryRun {
    Write-Step 'Dry run: installing hook only (no explorer.exe, no window changes).'

    $state = Get-ExplorerState
    Write-Step ("Existing Explorer windows: {0}, tabs: {1}" -f $state.Windows.Count, $state.Tabs.Count)
    foreach ($w in $state.Windows) {
        Write-Step ("  " + $w.ToString())
        if ($w.layered_alpha -eq 0) {
            Write-Warn ("baseline window 0x{0:X} is layered with alpha=0" -f $w.hwnd)
        }
    }

    $events = $null
    [WinEventTimeline]::Start()
    try {
        [WinEventTimeline]::Mark('dryrun-start')
        Start-Sleep -Milliseconds 1200
        [WinEventTimeline]::Mark('dryrun-end')
    }
    finally {
        $events = [WinEventTimeline]::StopAndDump()
    }

    Write-Step ("Hook installed and removed cleanly. Events captured: {0}" -f $events.Count)
    $marks = @($events | Where-Object { $_.evt -eq 'mark' })
    if ($marks.Count -ge 2) {
        $delta = $marks[$marks.Count - 1].t_us - $marks[0].t_us
        Write-Step ("Mark round-trip OK: {0:F0} us between dryrun-start and dryrun-end." -f $delta)
    } else {
        Write-Warn ("expected 2 marks, got {0}" -f $marks.Count)
    }

    if ($events.Count -gt 0) {
        Write-Step '--- timeline ---'
        foreach ($e in $events) { Write-Step ("  " + $e.ToString()) }
    }

    Write-Step ''
    Write-Step 'Dry run OK. No Explorer window was started and no window was touched.'
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

Initialize-TimelineType

if ($DryRun) {
    Invoke-DryRun
    return
}

if ($Runs -lt 1) { throw "-Runs must be >= 1 (got $Runs)." }
if ($PollMs -lt 5) { throw "-PollMs must be >= 5 (got $PollMs)." }
if ($TimeoutMs -lt 100) { throw "-TimeoutMs must be >= 100 (got $TimeoutMs)." }

$tool = @(Get-Process -Name $script:ToolProcessName -ErrorAction SilentlyContinue)
if ($tool.Count -eq 0) {
    Write-Warn "ExplorerTabUtility is not running: expect 'timeout' rows (this is valid for a Rust build without the C# tool)."
} else {
    Write-Step ("ExplorerTabUtility running: PID {0}" -f (($tool | ForEach-Object { $_.Id }) -join ', '))
}

if ($UniqueSubdir) {
    if ($PSBoundParameters.ContainsKey('Path')) {
        Write-Note "-UniqueSubdir is on: -Path '$Path' is ignored, per-run directories under '$ProbeRoot' are used instead."
    }
    New-Item -ItemType Directory -Force -Path $ProbeRoot | Out-Null
} else {
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        throw "Path does not exist or is not a directory: $Path"
    }
    if ($tool.Count -gt 0 -and $Runs -gt 1) {
        Write-Warn 'ReuseTabs is likely on: opening the same path again selects the existing tab and creates no new tab. Use the default -UniqueSubdir for C# baselines.'
    }
}

if (-not $OutCsv) {
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $OutCsv = Join-Path $PSScriptRoot ("bench-out\run-{0}.csv" -f $stamp)
} else {
    $OutCsv = [System.IO.Path]::GetFullPath($OutCsv)
}
$outDir = Split-Path -Parent $OutCsv
if ($outDir) { New-Item -ItemType Directory -Force -Path $outDir | Out-Null }
$csvBase = [System.IO.Path]::GetFileNameWithoutExtension($OutCsv)

$pre = Get-ExplorerState
if ($pre.Windows.Count -eq 0) {
    throw 'No pre-existing CabinetWClass window found. Open one Explorer window first; the harness refuses to guess which windows are the user''s.'
}

Write-Step ("Runs: {0}  timeout: {1} ms  poll: {2} ms  target mode: {3}" -f `
    $Runs, $TimeoutMs, $PollMs, $(if ($UniqueSubdir) { 'unique temp dir per run' } else { $Path }))
Write-Step ("CSV: {0}" -f $OutCsv)
Write-Step ''

$rows = [System.Collections.Generic.List[object]]::new()
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'

for ($i = 1; $i -le $Runs; $i++) {
    if ($UniqueSubdir) {
        $runPath = Join-Path $ProbeRoot ('run-{0}-{1:D2}' -f $stamp, $i)
        New-Item -ItemType Directory -Force -Path $runPath | Out-Null
    } else {
        $runPath = $Path
    }

    $row = Invoke-BenchRun -Index $i -Total $Runs -RunPath $runPath -CsvBaseName $csvBase -OutDir $outDir
    $rows.Add($row)

    if ($i -lt $Runs) {
        Start-Sleep -Milliseconds 700
    }
}

$rows | Export-Csv -LiteralPath $OutCsv -NoTypeInformation -Encoding utf8

Show-Summary -Rows $rows

Write-Step ("CSV written: {0}" -f $OutCsv)
if ($UniqueSubdir) {
    Write-Note ("Probe directories were left in place (a merged tab may still point at them): {0}" -f $ProbeRoot)
}
