# Pastes the clipboard into a hidden Excel workbook and prints what Excel
# stored in each cell: shown text and value type. Used to check that
# Slate's copies land in separate columns with their values intact.
param([int]$MaxRows = 12)
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false
$xl.DisplayAlerts = $false
try {
    $wb = $xl.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    [void]$ws.Range("A1").Select()
    $ws.Paste()
    $used = $ws.UsedRange
    "rows=$($used.Rows.Count) cols=$($used.Columns.Count)"
    for ($r = 1; $r -le [Math]::Min($used.Rows.Count, $MaxRows); $r++) {
        $cells = for ($c = 1; $c -le $used.Columns.Count; $c++) {
            $cell = $ws.Cells.Item($r, $c)
            $v = $cell.Value2
            $t = if ($null -eq $v) { '' } elseif ($v -is [double]) { 'n' } else { 's' }
            "$($cell.Text)[$t]"
        }
        ($cells -join ' | ')
    }
    $wb.Close($false)
} finally {
    $xl.Quit()
    [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl)
}
