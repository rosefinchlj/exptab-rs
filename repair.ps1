$p = 'E:\use\ai\deepseek\Rust\exptab-rs\src\win.rs'
$lines = [System.IO.File]::ReadAllLines($p)
$out = New-Object System.Collections.Generic.List[string]
foreach ($l in $lines) {
    $t = $l.TrimEnd()
    # correct indentation for lines that start flush-left inside a block
    if ($out.Count -gt 0) {
        $prev = $out[$out.Count - 1]
        $prevTrim = $prev.TrimEnd()
    }
    $t = [regex]::Replace($t, '^(pub |fn |unsafe |impl |static |use )', '    $1')
    $out.Add($t)
}
[System.IO.File]::WriteAllLines($p, $out, (New-Object System.Text.UTF8Encoding($false)))
