$env:RUSTPYTHONPATH = "$PWD\Lib"
Remove-Item Env:HTTP_PROXY, Env:HTTPS_PROXY, Env:ALL_PROXY -ErrorAction SilentlyContinue
$modules = @('test_pyexpat','test_xml_etree','test_sax','test_pydoc','test_inspect','test_coroutines','test_ast','test_trace','test_bytes','test_socket','test_warnings','test_hmac','test_xmlrpc','test_asynchat','test_smtplib','test_email')
foreach ($m in $modules) {
  if (-not (Test-Path "Lib\test\$m.py")) { "  {0,-18} (不存在)" -f $m; continue }
  $p = Start-Process -FilePath '.\target\release\rustpython.exe' -ArgumentList '-m','test',$m -WorkingDirectory $PWD -NoNewWindow -PassThru -RedirectStandardOutput 'bench\out.txt' -RedirectStandardError 'bench\err.txt'
  if (-not $p.WaitForExit(600000)) { $p.Kill(); "  {0,-18} TIMEOUT" -f $m; continue }
  $out = Get-Content bench\out.txt -Raw
  if ($out -match 'Tests result: SUCCESS') { $status = 'PASS' }
  elseif ($out -match 'FAILED \(([^)]+)\)') { $status = "FAIL ($($Matches[1]))" }
  elseif ($out -match 'NO TESTS RAN') { $status = 'SKIP' }
  else { $status = 'CRASH' }
  if ($out -match 'Total tests: run=(\d+)') { $run = $Matches[1] } else { $run = '?' }
  if ($out -match 'unexpected success') { $us = ' US!' } else { $us = '' }
  "  {0,-18} {1,-26} run={2}{3}" -f $m, $status, $run, $us
}
