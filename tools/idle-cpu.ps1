# Measure the daemon's idle CPU and per-thread split.
# Usage: tools\idle-cpu.ps1 [-Seconds 5] [-SampleSlices 1]
param(
    [int]$Seconds = 5
)

$p = Get-Process exptab -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $p) { Write-Output "no exptab process"; return }
Write-Output ("pid={0} threads={1} ws={2}MB private={3}MB" -f $p.Id, $p.Threads.Count,
    [math]::Round($p.WorkingSet64 / 1MB, 1), [math]::Round($p.PrivateMemorySize64 / 1MB, 1))

$before = @{}
foreach ($t in $p.Threads) { $before[$t.Id] = $t.TotalProcessorTime.TotalMilliseconds }
$c0 = $p.TotalProcessorTime.TotalMilliseconds
$t0 = Get-Date
Start-Sleep -Seconds $Seconds
$p.Refresh()
$c1 = $p.TotalProcessorTime.TotalMilliseconds
$wall = ((Get-Date) - $t0).TotalMilliseconds
Write-Output ("idle cpu: {0} ms over {1} ms wall = {2}% of one core" -f ($c1 - $c0), [math]::Round($wall, 0), [math]::Round(($c1 - $c0) / $wall * 100, 2))

Write-Output "per-thread (during sample):"
foreach ($t in ($p.Threads | Sort-Object TotalProcessorTime -Descending)) {
    $b = if ($before.ContainsKey($t.Id)) { $before[$t.Id] } else { 0 }
    $d = $t.TotalProcessorTime.TotalMilliseconds - $b
    Write-Output ("  tid={0} state={1} wait={2} sample={3}ms total={4}s" -f $t.Id, $t.ThreadState, $t.WaitReason,
        [math]::Round($d, 1), [math]::Round($t.TotalProcessorTime.TotalSeconds, 1))
}
