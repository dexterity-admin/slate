# Opens a big file in Slate and reports how long loading takes (until the
# process goes idle), peak and settled memory, then runs keyboard steps
# (as in drive.ps1) and times each one until Slate is idle again.
#   .\tools\bigfile.ps1 -File D:\big.log -Steps "k:0x23:1|shot:end"
param(
    [Parameter(Mandatory)] [string]$File,
    [string]$Exe = "$PSScriptRoot\..\target\release\slate.exe",
    [string]$OutDir = "$PSScriptRoot\..\shots",
    [string]$Steps = "",
    [switch]$Keep
)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class BW {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint msg, IntPtr wp, IntPtr lp);
  [DllImport("user32.dll")] public static extern bool UpdateWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
[void][BW]::SetProcessDPIAware()
$env:SLATE_DATA_DIR = "U:\Vikram\_Claude_Work\temp\slate-bigdata"
# Slate notes when loading and saving finish in this file.
$trace = Join-Path $env:TEMP "slate-bigfile-trace.txt"
Remove-Item $trace -ErrorAction SilentlyContinue
$env:SLATE_TRACE = $trace
function Wait-Trace([string]$what) {
    $t = [Diagnostics.Stopwatch]::StartNew()
    while ($t.ElapsedMilliseconds -lt 300000) {
        if ((Test-Path $trace) -and (Select-String -Path $trace -Pattern " $what$" -Quiet)) { return $t.ElapsedMilliseconds }
        Start-Sleep -Milliseconds 50
    }
    return -1
}
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $Exe -ArgumentList ("-multiInst -nosession `"$File`"") -PassThru
Remove-Item Env:SLATE_DATA_DIR
for ($i = 0; $i -lt 100 -and $p.MainWindowHandle -eq [IntPtr]::Zero; $i++) { Start-Sleep -Milliseconds 50; $p.Refresh() }
$hw = $p.MainWindowHandle
[void][BW]::SetWindowPos($hw, [IntPtr](-1), 60, 40, 1180, 760, 0)
"window after $($sw.ElapsedMilliseconds) ms"
# Waits until the process uses no CPU for a while; returns elapsed ms.
function Wait-Idle([int]$quietMs = 600) {
    $t = [Diagnostics.Stopwatch]::StartNew()
    $p.Refresh(); $last = $p.TotalProcessorTime.TotalMilliseconds; $quiet = 0; $peak = 0
    while ($t.ElapsedMilliseconds -lt 180000) {
        Start-Sleep -Milliseconds 100
        $p.Refresh()
        $peak = [Math]::Max($peak, $p.WorkingSet64)
        $c = $p.TotalProcessorTime.TotalMilliseconds
        if ($c - $last -lt 8) { $quiet += 100 } else { $quiet = 0 }
        $last = $c
        if ($quiet -ge $quietMs) { break }
    }
    return @([int]($t.ElapsedMilliseconds - $quiet), $peak)
}
[void](Wait-Trace "loaded")
$p.Refresh()
"loaded in $($sw.ElapsedMilliseconds) ms; working set $([math]::Round($p.WorkingSet64/1MB)) MB; private $([math]::Round($p.PrivateMemorySize64/1MB)) MB"
foreach ($s in $Steps.Split('|')) {
    if (-not $s) { continue }
    $kind, $arg = $s.Split(':', 2)
    switch ($kind) {
        'k' {
            $parts = $arg.Split(':')
            $vk = [Convert]::ToInt32($parts[0], $(if ($parts[0] -like '0x*') { 16 } else { 10 }))
            $m = if ($parts.Count -gt 1) { [int]$parts[1] } else { 0 }
            $t0 = [Diagnostics.Stopwatch]::StartNew()
            [void][BW]::SendMessageW($hw, 0x8002, [IntPtr]$vk, [IntPtr]$m)
            $r = Wait-Idle 400
            "  key ${arg}: $($t0.ElapsedMilliseconds - 400) ms, $([math]::Round($p.WorkingSet64/1MB)) MB"
        }
        'c' {
            $t0 = [Diagnostics.Stopwatch]::StartNew()
            foreach ($ch in $arg.ToCharArray()) { [void][BW]::SendMessageW($hw, 0x0102, [IntPtr][int][char]$ch, [IntPtr]::Zero) }
            $r = Wait-Idle 400
            "  typed '$arg': $($t0.ElapsedMilliseconds - 400) ms"
        }
        'w' { Start-Sleep -Milliseconds ([int]$arg) }
        'saved' { $ms = Wait-Trace "saved.*"; "  save finished after $ms ms: " + ((Get-Content $trace | Select-String 'saved') -join ' ') }
        'shot' {
            [void][BW]::UpdateWindow($hw)
            $rc = New-Object BW+RECT; [void][BW]::GetWindowRect($hw, [ref]$rc)
            $bmp = New-Object System.Drawing.Bitmap(($rc.R - $rc.L), ($rc.B - $rc.T))
            $g = [System.Drawing.Graphics]::FromImage($bmp)
            $hdc = $g.GetHdc(); [void][BW]::PrintWindow($hw, $hdc, 2); $g.ReleaseHdc($hdc)
            $bmp.Save((Join-Path $OutDir "$arg.png")); $g.Dispose(); $bmp.Dispose()
            "  saved $arg.png"
        }
    }
}
$p.Refresh()
"final: working set $([math]::Round($p.WorkingSet64/1MB)) MB, private $([math]::Round($p.PrivateMemorySize64/1MB)) MB, CPU $([math]::Round($p.TotalProcessorTime.TotalSeconds,1)) s"
if (-not $Keep) { Stop-Process -Id $p.Id -Force }
