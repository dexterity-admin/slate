# Unsaved work must survive anything that ends Slate: a crash or power cut
# (the process killed), Windows shutting down (its end-session messages, then
# the process killed, as Windows does), with session restore on or off.
# Each case edits files, ends Slate that way, starts it again and checks from
# session.ini and the backups that every tab and every edit came back.
#   .\tools\recovery-test.ps1
param(
    [string]$Exe = "$PSScriptRoot\..\target\release\slate.exe",
    [string]$Work = (Join-Path $env:TEMP "slate-recovery-test"),
    [int]$Files = 100
)
$ErrorActionPreference = "Stop"
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class RT {
  [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint msg, IntPtr wp, IntPtr lp);
}
"@
$Exe = (Resolve-Path $Exe).Path
Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue
$data = Join-Path $Work "data"
$dir = Join-Path $Work "files"
New-Item -ItemType Directory -Force $data, $dir | Out-Null
$env:SLATE_DATA_DIR = $data
$paths = 1..$Files | ForEach-Object {
    $f = Join-Path $dir ("f{0:D3}.txt" -f $_)
    [IO.File]::WriteAllText($f, "line one of file $_`r`nline two`r`n")
    $f
}
$fails = 0
function Check([bool]$ok, [string]$what) {
    if ($ok) { Write-Output "  ok    $what" } else { Write-Output "  FAIL  $what"; $script:fails++ }
}

function Start-Slate([string[]]$more = @()) {
    $al = @("-multiInst") + ($more | ForEach-Object { '"' + $_ + '"' })
    $p = Start-Process -FilePath $Exe -ArgumentList $al -PassThru
    for ($i = 0; $i -lt 100; $i++) {
        Start-Sleep -Milliseconds 100
        $p.Refresh()
        if ($p.MainWindowHandle -ne [IntPtr]::Zero) { break }
    }
    Start-Sleep -Milliseconds 800
    $p
}
function Type-Text($p, [string]$s) { foreach ($ch in $s.ToCharArray()) { [void][RT]::SendMessageW($p.MainWindowHandle, 0x0102, [IntPtr][int][char]$ch, [IntPtr]::Zero) } }
# Keys through Slate's test hook: modifiers 1 ctrl, 2 shift, 4 alt.
function Key($p, [int]$vk, [int]$mods = 0) { [void][RT]::SendMessageW($p.MainWindowHandle, 0x8002, [IntPtr]$vk, [IntPtr]$mods) }
function Crash($p) { Stop-Process -Id $p.Id -Force; $p.WaitForExit(10000) | Out-Null }
# What Windows does at shutdown: ask, announce the end, then end the process.
function Shutdown($p) {
    [void][RT]::SendMessageW($p.MainWindowHandle, 0x0011, [IntPtr]::Zero, [IntPtr]::Zero)
    [void][RT]::SendMessageW($p.MainWindowHandle, 0x0016, [IntPtr]1, [IntPtr]::Zero)
    Crash $p
}
function Close-Clean($p) {
    [void][RT]::SendMessageW($p.MainWindowHandle, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
    if (-not $p.WaitForExit(15000)) { Write-Output "  (did not close; killed)"; Crash $p }
}
# Tabs listed in session.ini, each with the text it would reopen with.
function Session-Tabs {
    $tabs = @()
    $cur = $null
    foreach ($l in [IO.File]::ReadAllLines((Join-Path $data "session.ini"))) {
        if ($l -eq "[tab]") { $cur = @{}; $tabs += $cur; continue }
        if ($cur -and $l -match '^(\w+)=(.*)$') { $cur[$matches[1]] = $matches[2] }
    }
    foreach ($t in $tabs) {
        $bk = if ($t.backup) { Join-Path $data "backup\$($t.backup)" }
        $t.text = if ($bk -and (Test-Path $bk)) { [IO.File]::ReadAllText($bk) } elseif ($t.path) { [IO.File]::ReadAllText($t.path) } else { "" }
    }
    , $tabs
}
$marker = Join-Path $data "running"

Write-Output "1. Crash with $Files files open, every one edited, plus a new tab"
$p = Start-Slate $paths
Key $p 0x24 1   # Ctrl+Home on the first tab
for ($k = 1; $k -le $Files; $k++) {
    # The active tab after Ctrl+Tab; tabs open in the order given.
    Type-Text $p "E$k~"
    Key $p 0x09 1
}
Key $p 0x4E 1
Type-Text $p "NEWTAB-UNSAVED"
Start-Sleep -Seconds 3
Crash $p
Check (Test-Path $marker) "the running marker is left behind"
$p = Start-Slate
Start-Sleep -Seconds 3
Close-Clean $p
Check (-not (Test-Path $marker)) "a normal close removes the marker"
$tabs = Session-Tabs
Check ($tabs.Count -eq $Files + 1) "all $($Files + 1) tabs came back (found $($tabs.Count))"
$missing = 1..$Files | Where-Object { $k = $_; -not ($tabs | Where-Object { $_.text -like "*E$k~*" }) }
Check ($missing.Count -eq 0) "every file's unsaved edit came back (missing: $($missing -join ','))"
Check ([bool]($tabs | Where-Object { $_.text -eq "NEWTAB-UNSAVED" })) "the never-saved new tab came back"

Write-Output "2. Windows shutdown straight after typing (before the 2 s backup)"
$p = Start-Slate
Type-Text $p "SHUTDOWN-EDIT"
Shutdown $p
$p = Start-Slate
Start-Sleep -Seconds 3
Close-Clean $p
$tabs = Session-Tabs
Check ($tabs.Count -eq $Files + 1) "all tabs came back after shutdown (found $($tabs.Count))"
Check ([bool]($tabs | Where-Object { $_.text -like "*SHUTDOWN-EDIT*" })) "the edit made just before shutdown came back"

Write-Output "3. A file saved just before a crash reopens from disk"
$p = Start-Slate
Key $p 0x09 1   # from the new tab (last) to the first file
Type-Text $p "SAVED-THEN-CRASH"
Key $p 0x53 1   # Ctrl+S
Start-Sleep -Seconds 3
Crash $p
$p = Start-Slate
Start-Sleep -Seconds 3
Close-Clean $p
$tabs = Session-Tabs
Check ($tabs.Count -eq $Files + 1) "no tab lost (found $($tabs.Count))"
$saved = $tabs | Where-Object { $_.text -like "*SAVED-THEN-CRASH*" }
Check ($saved -and $saved.path -and (([IO.File]::ReadAllText($saved.path)) -like "*SAVED-THEN-CRASH*")) "the saved text is in the file on disk and in the tab"

Write-Output "4. Unsaved text the session does not list is recovered, not deleted"
$orphan = "lost_note.txt@1700000000000-9999.txt"
[IO.File]::WriteAllText((Join-Path $data "backup\$orphan"), "ORPHAN-TEXT")
[IO.File]::WriteAllText((Join-Path $data "backup\half.txt.part"), "cut short")
$p = Start-Slate
Start-Sleep -Seconds 3
Close-Clean $p
$tabs = Session-Tabs
Check ([bool]($tabs | Where-Object { $_.text -eq "ORPHAN-TEXT" })) "the unlisted backup opened as a tab"
Check (-not (Test-Path (Join-Path $data "backup\half.txt.part"))) "a half-written copy is cleared away"

Write-Output "5. Session restore turned off: a crash still loses nothing"
# Someone who keeps it off: no session or backups to start from.
$s = Join-Path $data "settings.ini"
[IO.File]::WriteAllText($s, ([IO.File]::ReadAllText($s) -replace 'restore_session=1', 'restore_session=0'))
Remove-Item (Join-Path $data "session.ini"), (Join-Path $data "backup\*") -Force
$p = Start-Slate @($paths[4])
Type-Text $p "RESTORE-OFF-FILE"
Key $p 0x4E 1
Type-Text $p "RESTORE-OFF-NEW"
Start-Sleep -Seconds 3
Crash $p
$p = Start-Slate
Start-Sleep -Seconds 3
Crash $p   # a normal close would ask about each unsaved file
$tabs = Session-Tabs
Check ([bool]($tabs | Where-Object { $_.text -like "*RESTORE-OFF-FILE*" -and $_.path -like "*f005.txt" })) "the edited file came back despite the setting"
Check ([bool]($tabs | Where-Object { $_.text -eq "RESTORE-OFF-NEW" })) "the new tab came back despite the setting"
[IO.File]::WriteAllText($s, ([IO.File]::ReadAllText($s) -replace 'restore_session=0', 'restore_session=1'))

Write-Output "6. A file over 64 MB is copied at shutdown"
$big = Join-Path $dir "big.log"
$line = [Text.Encoding]::ASCII.GetBytes(("x" * 99) + "`n")
$fs = [IO.File]::Create($big)
for ($i = 0; $i -lt 700000; $i++) { $fs.Write($line, 0, $line.Length) }
$fs.Close()
$p = Start-Slate @($big)
Start-Sleep -Seconds 6
Key $p 0x24 1
Type-Text $p "BIG-EDIT"
Shutdown $p
$p = Start-Slate
Start-Sleep -Seconds 8
Crash $p
$tabs = Session-Tabs
$b = $tabs | Where-Object { $_.path -like "*big.log" }
Check ($b -and $b.backup -and $b.text.StartsWith("BIG-EDIT")) "the big file's unsaved edit came back"

Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue
if ($fails) { Write-Output "$fails check(s) FAILED"; exit 1 } else { Write-Output "All checks passed" }
