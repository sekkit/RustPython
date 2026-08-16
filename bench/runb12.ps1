$ErrorActionPreference = 'Continue'
$env:RUSTPYTHONPATH = "$PWD\Lib;$PWD\bench\labs"
Remove-Item Env:HTTP_PROXY, Env:HTTPS_PROXY, Env:ALL_PROXY -ErrorActionSilentlyContinue
$modules = @('test_cprofile','test_profile','test_monitoring','test_builtin','test_compile','test_code','test_importlib','test_ctypes','test_sysconfig','test_venv','test_signal','test_threading','test_mimetypes','test_winreg')
foreach ($m in $modules) {
  $log = "bench\r12_$m.log"
  $p = Start-Process -FilePath '.\target\release\rustpython.exe' -ArgumentList '-m','test',$m -WorkingDirectory $PWD -NoNewWindow -PassThru -RedirectStandardOutput $log -RedirectStandardError "$log.err"
  if (-not $p.WaitForExit(900000)) { $p.Kill(); "  {0,-18} TIMEOUT" -f $m; continue }
  Start-Sleep -Milliseconds 200
  $out = Get-Content $log -Raw
  if ($out -match 'Tests result: SUCCESS') { $status = 'PASS' }
  elseif ($out -match 'FAILED \(([^)]+)\)') { $status = "FAIL ($($Matches[1]))" }
  else { $status = 'CRASH' }
  if ($out -match 'Total tests: run=([\d,]+)') { $run = $Matches[1] } else { $run = '?' }
  "  {0,-18} {1,-30} run={2}" -f $m, $status, $run
}
