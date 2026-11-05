# Drives two Slate windows side by side as two team members on the LAN
# (they find each other by themselves), and captures both in one screenshot.
# Steps (separated by '|'), each for window a or b unless noted:
#   a:c:TEXT  a:k:VK[:MODS]  a:click:X,Y      as in drive.ps1
#   w:MS                                       wait
#   shot:NAME                                  screenshot of both windows
param(
    [string]$Exe = "$PSScriptRoot\..\target\release\slate.exe",
    [string]$ArgsA = "",
    [string]$ArgsB = "",
    [string]$NameA = "Ravi Kumar",
    [string]$NameB = "Anil Shah",
    # Separate data folders (library, settings) per window, off C:.
    [string]$DataA = "",
    [string]$DataB = "",
    [string]$OutDir = "$PSScriptRoot\..\shots",
    [string]$Steps = "shot:duo",
    [int]$Width = 860,
    [int]$Height = 640,
    [switch]$Keep
)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class DW {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint msg, IntPtr wp, IntPtr lp);
  [DllImport("user32.dll")] public static extern bool UpdateWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
[void][DW]::SetProcessDPIAware()
New-Item -ItemType Directory -Force $OutDir | Out-Null
function Start-One([string]$name, [string]$extra, [int]$x, [string]$data) {
    $env:SLATE_TEAM_NAME = $name
    if ($data) { $env:SLATE_DATA_DIR = $data } else { Remove-Item Env:SLATE_DATA_DIR -ErrorAction SilentlyContinue }
    $p = Start-Process -FilePath $Exe -ArgumentList ("-multiInst -nosession " + $extra) -PassThru
    for ($i = 0; $i -lt 60 -and $p.MainWindowHandle -eq [IntPtr]::Zero; $i++) { Start-Sleep -Milliseconds 100; $p.Refresh() }
    [void][DW]::SetWindowPos($p.MainWindowHandle, [IntPtr](-1), $x, 40, $Width, $Height, 0)
    $p
}
$pa = Start-One $NameA $ArgsA 0 $DataA
$pb = Start-One $NameB $ArgsB ($Width + 8) $DataB
Remove-Item Env:SLATE_DATA_DIR -ErrorAction SilentlyContinue
Remove-Item Env:SLATE_TEAM_NAME
Start-Sleep -Milliseconds 800
function MkLp([int]$x, [int]$y) { [IntPtr](($y -shl 16) -bor ($x -band 0xFFFF)) }
foreach ($s in $Steps.Split('|')) {
    $who, $rest = $s.Split(':', 2)
    if ($who -eq 'w') { Start-Sleep -Milliseconds ([int]$rest); continue }
    if ($who -eq 'shot') {
        Start-Sleep -Milliseconds 350
        foreach ($h in $pa.MainWindowHandle, $pb.MainWindowHandle) { [void][DW]::UpdateWindow($h) }
        $ra = New-Object DW+RECT; $rb = New-Object DW+RECT
        $okA = [DW]::GetWindowRect($pa.MainWindowHandle, [ref]$ra); $okB = [DW]::GetWindowRect($pb.MainWindowHandle, [ref]$rb)
        if (-not $okA) { $ra = $rb }; if (-not $okB) { $rb = $ra }
        $bw = $rb.R - $ra.L; $bh = [Math]::Max($ra.B, $rb.B) - $ra.T
        $bmp = New-Object System.Drawing.Bitmap($bw, $bh)
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($ra.L, $ra.T, 0, 0, (New-Object System.Drawing.Size($bw, $bh)))
        $out = Join-Path $OutDir "$rest.png"
        $bmp.Save($out); $g.Dispose(); $bmp.Dispose()
        Write-Output "saved $out"
        continue
    }
    $hw = if ($who -eq 'a') { $pa.MainWindowHandle } else { $pb.MainWindowHandle }
    $kind, $arg = $rest.Split(':', 2)
    switch ($kind) {
        'c' { foreach ($ch in $arg.ToCharArray()) { [void][DW]::SendMessageW($hw, 0x0102, [IntPtr][int][char]$ch, [IntPtr]::Zero) } }
        'k' {
            $parts = $arg.Split(':')
            $vk = [Convert]::ToInt32($parts[0], $(if ($parts[0] -like '0x*') { 16 } else { 10 }))
            $m = if ($parts.Count -gt 1) { [int]$parts[1] } else { 0 }
            [void][DW]::SendMessageW($hw, 0x8002, [IntPtr]$vk, [IntPtr]$m)
        }
        'kill' { Stop-Process -Id $(if ($who -eq 'a') { $pa.Id } else { $pb.Id }) -Force }
        'close' { [void][DW]::SendMessageW($hw, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
        'click' { $x, $y = $arg.Split(','); [void][DW]::SendMessageW($hw, 0x0201, [IntPtr]1, (MkLp $x $y)); [void][DW]::SendMessageW($hw, 0x0202, [IntPtr]0, (MkLp $x $y)) }
    }
}
if (-not $Keep) {
    foreach ($p in $pa, $pb) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
}
