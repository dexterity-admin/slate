# Process-level test of the in-app updater's swap, handshake and rollback.
# For each case a scratch install of slate.exe "updates" itself to a local
# file through the real code path (SLATE_TEST_UPDATE):
#   crash - the new exe exits at once        -> the old exe must come back
#   hang  - the new exe runs but never reports -> killed after 25 s, rolled back
#   good  - a working Slate                   -> it takes over, the old one exits
param(
    [string]$Exe = "$PSScriptRoot\..\target\release\slate.exe",
    [string]$Work = "$PSScriptRoot\..\tmp\rollback-test"
)
$ErrorActionPreference = "Stop"
Remove-Item $Work -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $Work | Out-Null
$orig = (Get-FileHash $Exe).Hash
# Stand-ins for broken versions: a program that exits straight away with
# Slate's arguments, and one that stays open without reporting in.
$crash = "$env:WINDIR\System32\whoami.exe"
$hang = "$env:WINDIR\System32\notepad.exe"
$cases = @(
    @{ name = "crash"; update = $crash; wait = 6 },
    @{ name = "hang"; update = $hang; wait = 32 },
    @{ name = "good"; update = (Resolve-Path $Exe).Path; wait = 8 }
)
$fail = 0
foreach ($c in $cases) {
    $dir = Join-Path $Work $c.name
    New-Item -ItemType Directory -Force "$dir\data" | Out-Null
    Copy-Item $Exe "$dir\slate.exe"
    $env:SLATE_DATA_DIR = "$dir\data"
    $env:SLATE_TEST_UPDATE = $c.update
    $p = Start-Process -FilePath "$dir\slate.exe" -ArgumentList "-multiInst" -PassThru
    Remove-Item Env:SLATE_TEST_UPDATE, Env:SLATE_DATA_DIR
    Start-Sleep -Seconds $c.wait
    $now = (Get-FileHash "$dir\slate.exe").Hash
    $old = Test-Path "$dir\slate.exe.old"
    $p.Refresh()
    $alive = -not $p.HasExited
    $visible = $alive -and ($p.MainWindowHandle -ne [IntPtr]::Zero)
    $kids = Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -eq "$dir\slate.exe" -and $_.ProcessId -ne $p.Id }
    switch ($c.name) {
        { $_ -in "crash", "hang" } {
            $ok = ($now -eq $orig) -and $alive -and $visible -and -not $old
            $stray = Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -eq "$dir\slate.exe" -and $_.ProcessId -ne $p.Id }
            "{0,-6} rolled back: exe restored={1} old window back={2} .old left={3} stray processes={4} -> {5}" -f $c.name, ($now -eq $orig), $visible, $old, @($stray).Count, $(if ($ok -and @($stray).Count -eq 0) { "PASS" } else { "FAIL" })
            if (-not $ok -or @($stray).Count -gt 0) { $fail++ }
        }
        "good" {
            $ok = (-not $alive) -and (@($kids).Count -eq 1)
            "{0,-6} handed over: old exited={1} new running={2} -> {3}" -f $c.name, (-not $alive), (@($kids).Count -eq 1), $(if ($ok) { "PASS" } else { "FAIL" })
            if (-not $ok) { $fail++ }
            foreach ($k in $kids) { Stop-Process -Id $k.ProcessId -Force -ErrorAction SilentlyContinue }
        }
    }
    if ($alive) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
    Get-Process notepad -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq "$dir\slate.exe" } | Stop-Process -Force
}
"failures: $fail"
