$ErrorActionPreference = 'Continue'
$env:RUSTPYTHONPATH = "$PWD\Lib;$PWD\bench\labs"
Remove-Item Env:HTTP_PROXY, Env:HTTPS_PROXY, Env:ALL_PROXY -ErrorAction SilentlyContinue
$modules = @(
 'test_slice','test_cmd','test_shutil','test_tempfile','test_pathlib','test_io','test_csv',
 'test_urllib','test_xml','test_email','test_argparse','test_logging','test_json','test_bytes'
)
foreach ($m in $modules) {
  $log = "bench\r14b_$m.log"
  $p = Start-Process -FilePath '.\target\release\rustpython.exe' -ArgumentList '-m','test',$m -WorkingDirectory $PWD -NoNewWindow -PassThru -RedirectStandardOutput $log -RedirectStandardError "$log.err"
  if (-not $p.WaitForExit(900000)) { $p.Kill(); "  {0,-16} TIMEOUT" -f $m; continue }
  Start-Sleep -Milliseconds 200
  $out = Get-Content $log -Raw
  if ($out -match 'Tests result: SUCCESS') { $status = 'PASS' }
  elseif ($out -match 'FAILED \(([^)]+)\)') { $status = "FAIL ($($Matches[1]))" }
  else { $status = 'CRASH' }
  if ($out -match 'Total tests: run=([\d,]+)') { $run = $Matches[1] } else { $run = '?' }
  "  {0,-16} {1,-30} run={2}" -f $m, $status, $run
}
