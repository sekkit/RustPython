$env:RUSTPYTHONPATH = "$PWD\Lib"
Remove-Item Env:HTTP_PROXY, Env:HTTPS_PROXY, Env:ALL_PROXY -ErrorAction SilentlyContinue
$modules = @('test_math','test_str','test_ctypes','test_sqlite3','test_importlib','test_descr','test_threading','test_ssl','test_json','test_os')
foreach ($m in $modules) {
  $p = Start-Process -FilePath '.\target\release\rustpython.exe' -ArgumentList '-m','test',$m -WorkingDirectory $PWD -NoNewWindow -PassThru -RedirectStandardOutput 'bench\out.txt' -RedirectStandardError 'bench\err.txt'
  if (-not $p.WaitForExit(600000)) { $p.Kill(); "  {0,-14} TIMEOUT" -f $m; continue }
  $out = Get-Content bench\out.txt -Raw
  if ($out -match 'Tests result: SUCCESS') { $status = 'PASS' }
  elseif ($out -match 'FAILED \(([^)]+)\)') { $status = "FAIL ($($Matches[1]))" }
  else { $status = 'CRASH' }
  if ($out -match 'Total tests: run=(\d+)') { $run = $Matches[1] } else { $run = '?' }
  "  {0,-14} {1,-24} run={2}" -f $m, $status, $run
}
