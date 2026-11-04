# Resource benchmark: Slate vs Notepad++ on the same files.
# Reports WorkingSet64, PrivateMemorySize64 and idle CPU (% of one core) over
# two windows: 4-12 s (caret still blinking) and 16-26 s (steady idle).
param(
    [string]$Slate = "$PSScriptRoot\..\target\release\slate.exe",
    [string]$Npp = "C:\Program Files\Notepad++\notepad++.exe",
    [string]$Data = "$PSScriptRoot\..\data",
    [string]$Big = "$PSScriptRoot\..\tmp\big.rs"
)
$Sample = "$PSScriptRoot\..\tmp\sample.rs"
New-Item -ItemType Directory -Force (Split-Path $Sample) | Out-Null
if (-not (Test-Path $Sample)) {
    # 20,000 synthetic Rust lines, about 2 MB.
    $sb = New-Object System.Text.StringBuilder
    for ($i = 1; $i -le 20000; $i++) {
        [void]$sb.Append(("line {0:D6} fn process_item_{1}(ctx, opts) -> Result<usize> {{ let v = compute(i) + offset; return ok(v); }}`n" -f $i, $i))
    }
    [IO.File]::WriteAllText($Sample, $sb.ToString())
}
if (-not (Test-Path $Big)) {
    $s = [IO.File]::ReadAllBytes($Sample)
    $f = [IO.File]::Create($Big); 1..5 | % { $f.Write($s, 0, $s.Length) }; $f.Close()
}

function Measure-One([string]$exe, [string]$argsPre, [string]$file, [string]$label) {
    $p = Start-Process -FilePath $exe -ArgumentList "$argsPre `"$file`"" -PassThru
    Start-Sleep -Milliseconds 4000
    $p.Refresh(); $c1 = $p.TotalProcessorTime.TotalMilliseconds
    Start-Sleep -Milliseconds 8000
    $p.Refresh(); $c2 = $p.TotalProcessorTime.TotalMilliseconds
    Start-Sleep -Milliseconds 4000
    $p.Refresh(); $c3 = $p.TotalProcessorTime.TotalMilliseconds
    Start-Sleep -Milliseconds 10000
    $p.Refresh(); $c4 = $p.TotalProcessorTime.TotalMilliseconds
    $ws = [math]::Round($p.WorkingSet64 / 1MB, 2)
    $pr = [math]::Round($p.PrivateMemorySize64 / 1MB, 2)
    $startup = [math]::Round($c1, 0)
    $blink = [math]::Round(($c2 - $c1) / 8000 * 100, 2)
    $steady = [math]::Round(($c4 - $c3) / 10000 * 100, 2)
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
    "{0,-26} WS={1,7} MB  Private={2,7} MB  startupCPU={3,5} ms  idle(4-12s)={4,5}%  idle(16-26s)={5,5}%" -f $label, $ws, $pr, $startup, $blink, $steady
}

$files = @(("$Data\empty.txt", "empty"), ($Sample, "2 MB"), ($Big, "10 MB"))
foreach ($f in $files) { Measure-One $Slate "-multiInst -nosession" $f[0] ("Slate " + $f[1]) }
if (Test-Path $Npp) {
    foreach ($f in $files) { Measure-One $Npp "-multiInst -nosession -notabbar" $f[0] ("Notepad++ " + $f[1]) }
}

