# P0 probe runner: temporarily stops the C# ExplorerTabUtility so it does not
# interfere with the test windows, runs the Rust probe, then restarts it.
param(
    [string]$Exe = "E:\use\ai\deepseek\Rust\exptab-rs\target\debug\exptab.exe",
    [string]$Log = "E:\use\ai\deepseek\Rust\exptab-rs\target\probe-last.log"
)
$ErrorActionPreference = "Continue"
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$tool = "D:\Program Files\ExplorerTabUtility\ExplorerTabUtility\ExplorerTabUtility.exe"
$toolProc = Get-Process ExplorerTabUtility -ErrorAction SilentlyContinue
$wasRunning = $null -ne $toolProc
if ($wasRunning) {
    Write-Host "[p0] stopping ExplorerTabUtility (pid $($toolProc.Id))"
    $toolProc | Stop-Process -Force
    Start-Sleep -Milliseconds 600
} else {
    Write-Host "[p0] ExplorerTabUtility not running"
}

try {
    & $Exe probe 2>&1 | Tee-Object -FilePath $Log
    Write-Host "[p0] probe exit code: $LASTEXITCODE"
} finally {
    if ($wasRunning) {
        Write-Host "[p0] restarting ExplorerTabUtility"
        Start-Process -FilePath $tool | Out-Null
        Start-Sleep -Milliseconds 800
        Get-Process ExplorerTabUtility -ErrorAction SilentlyContinue |
            Select-Object Id, ProcessName, StartTime | Format-Table | Out-String | Write-Host
    }
}
