# Drives a live Slate window and captures screenshots.
# Steps (separated by '|'):
#   c:TEXT        type characters (WM_CHAR)
#   k:VK[:MODS]   key via the test hook; MODS bits 1=ctrl 2=shift 4=alt (VK hex or decimal)
#   click:X,Y     left click at client pixel X,Y
#   dclick:X,Y    double click
#   move:X,Y      mouse move
#   hover:X,Y     put the real cursor there (tooltips, hover states)
#   w:MS          wait
#   shot:NAME     screenshot to <OutDir>\NAME.png
param(
    [string]$Exe = "$PSScriptRoot\..\target\release\slate.exe",
    [string]$ArgList = "",
    [string]$OutDir = "$PSScriptRoot\..\shots",
    [string]$Steps = "shot:start",
    [int]$Width = 1180,
    [int]$Height = 760,
    [switch]$Keep,
    [switch]$RealSession,
    [switch]$Attach
)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class SW {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint msg, IntPtr wp, IntPtr lp);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool UpdateWindow(IntPtr h);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string c, string t);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
[void][SW]::SetProcessDPIAware()
New-Item -ItemType Directory -Force $OutDir | Out-Null
$all = $(if ($RealSession) { $ArgList } else { "-multiInst -nosession " + $ArgList })
$p = if ($Attach) { Get-Process slate | Select -First 1 } elseif ($all.Trim()) { Start-Process -FilePath $Exe -ArgumentList $all -PassThru } else { Start-Process -FilePath $Exe -PassThru }
$hw = [IntPtr]::Zero
for ($i = 0; $i -lt 50 -and $hw -eq [IntPtr]::Zero; $i++) { Start-Sleep -Milliseconds 100; $hw = [SW]::FindWindowW("SlateEditorWindow", [NullString]::Value) }
[void][SW]::SetWindowPos($hw, [IntPtr](-1), 120, 60, $Width, $Height, 0)
[void][SW]::SetForegroundWindow($hw)
Start-Sleep -Milliseconds 700

function MkLp([int]$x, [int]$y) { [IntPtr](($y -shl 16) -bor ($x -band 0xFFFF)) }
foreach ($s in $Steps.Split('|')) {
    $kind, $arg = $s.Split(':', 2)
    switch ($kind) {
        'c' { foreach ($ch in $arg.ToCharArray()) { [void][SW]::SendMessageW($hw, 0x0102, [IntPtr][int][char]$ch, [IntPtr]::Zero) } }
        'k' {
            $parts = $arg.Split(':')
            $vk = [Convert]::ToInt32($parts[0], $(if ($parts[0] -like '0x*') { 16 } else { 10 }))
            $m = if ($parts.Count -gt 1) { [int]$parts[1] } else { 0 }
            [void][SW]::SendMessageW($hw, 0x8002, [IntPtr]$vk, [IntPtr]$m)
        }
        'click' { $x, $y = $arg.Split(','); [void][SW]::SendMessageW($hw, 0x0201, [IntPtr]1, (MkLp $x $y)); [void][SW]::SendMessageW($hw, 0x0202, [IntPtr]0, (MkLp $x $y)) }
        'dclick' { $x, $y = $arg.Split(','); foreach ($n in 1..2) { [void][SW]::SendMessageW($hw, 0x0201, [IntPtr]1, (MkLp $x $y)); [void][SW]::SendMessageW($hw, 0x0202, [IntPtr]0, (MkLp $x $y)) } }
        'move' { $x, $y = $arg.Split(','); [void][SW]::SendMessageW($hw, 0x0200, [IntPtr]0, (MkLp $x $y)) }
        # The real cursor, for hover effects (a sent move is undone by WM_MOUSELEAVE).
        'hover' { $x, $y = $arg.Split(','); $pt = New-Object SW+POINT; $pt.X = [int]$x; $pt.Y = [int]$y; [void][SW]::ClientToScreen($hw, [ref]$pt); [void][SW]::SetForegroundWindow($hw); [void][SW]::SetCursorPos($pt.X, $pt.Y) }
        'close' { [void][SW]::SendMessageW($hw, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero); Start-Sleep -Milliseconds 500 }
        'w' { Start-Sleep -Milliseconds ([int]$arg) }
        'max' { [void][SW]::SendMessageW($hw, 0x0112, [IntPtr]0xF030, [IntPtr]::Zero); Start-Sleep -Milliseconds 400 }
        'shot' {
            Start-Sleep -Milliseconds 350
            [void][SW]::UpdateWindow($hw)
            $r = New-Object SW+RECT
            [void][SW]::GetWindowRect($hw, [ref]$r)
            $bw = $r.R - $r.L; $bh = $r.B - $r.T
            $bmp = New-Object System.Drawing.Bitmap($bw, $bh)
            $g = [System.Drawing.Graphics]::FromImage($bmp)
            # The window's own pixels, whatever is on top of it.
            $hdc = $g.GetHdc(); [void][SW]::PrintWindow($hw, $hdc, 2); $g.ReleaseHdc($hdc)
            $out = Join-Path $OutDir "$arg.png"
            $bmp.Save($out); $g.Dispose(); $bmp.Dispose()
            Write-Output "saved $out"
        }
    }
}
if (-not $Keep) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }






