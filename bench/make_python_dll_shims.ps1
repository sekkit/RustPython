# make_python_dll_shims.ps1
#
# Generates python311.dll / python3.dll shims next to rustpython.exe.
# Extensions (.pyd) import the CPython DLL by name (e.g. python311.dll);
# these shims forward every symbol to rustpython.exe, which exports the
# full RustPython C API. Forwarding keeps all calls in the exe's module, so
# they share its thread-local VM.
#
# Also generates rustpython.lib (import lib for the exe) and python311.lib /
# python3.lib (so .pyds can be linked against the shims).
#
# Usage:  powershell -File bench\make_python_dll_shims.ps1 [-TargetDir <dir>]
#
# Requires: a Visual Studio toolchain and a release build with the `capi`
# feature (default), which makes rustpython.exe export the C-API symbols.

param(
    [string]$TargetDir = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
if ($TargetDir -eq "") {
    $TargetDir = Join-Path $root "target\release"
}
$exe = Join-Path $TargetDir "rustpython.exe"
if (-not (Test-Path $exe)) {
    Write-Error "rustpython.exe not found in $TargetDir - run `cargo build --release` first"
}

# Locate MSVC tools.
$vsRoot = "C:\Program Files\Microsoft Visual Studio"
$vcvars = Get-ChildItem $vsRoot -Directory -ErrorAction SilentlyContinue |
    ForEach-Object { Get-ChildItem $_.FullName -Directory -ErrorAction SilentlyContinue } |
    ForEach-Object { Join-Path $_.FullName "VC\Auxiliary\Build\vcvars64.bat" } |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $vcvars) {
    Write-Error "vcvars64.bat not found under $vsRoot"
}
$msvcBin = Get-ChildItem $vsRoot -Directory -ErrorAction SilentlyContinue |
    ForEach-Object { Get-ChildItem $_.FullName -Directory -ErrorAction SilentlyContinue } |
    ForEach-Object { Get-ChildItem (Join-Path $_.FullName "VC\Tools\MSVC") -Directory -ErrorAction SilentlyContinue } |
    Sort-Object Name -Descending | Select-Object -First 1
if (-not $msvcBin) {
    Write-Error "MSVC toolset not found"
}
$binDir = Join-Path $msvcBin.FullName "bin\Hostx64\x64"
$dumpbin = Join-Path $binDir "dumpbin.exe"
$libexe = Join-Path $binDir "lib.exe"

# Run a command inside the VS developer environment.
function Invoke-VsEnv([string]$command) {
    cmd /c "call `"$vcvars`" >nul 2>&1 && $command"
}

# 1. Collect export names from rustpython.exe.
$exports = & $dumpbin /EXPORTS $exe | ForEach-Object {
    if ($_ -match '^\s+\d+\s+\w+\s+[0-9A-Fa-f]+\s+(\S+)\s*=\s*\S+\s*$') {
        $matches[1]
    }
} | Sort-Object -Unique
if (-not $exports) {
    Write-Error "no exports found in $exe (was it built with the capi feature?)"
}
Write-Host "forwarding $($exports.Count) symbols from $exe"

# 2. Import library for the exe (so the forwarder shims can link).
$exeDef = Join-Path $TargetDir "rustpython.def"
$exeDefLines = @("LIBRARY rustpython", "EXPORTS")
foreach ($name in $exports) {
    $exeDefLines += "    $name"
}
Set-Content -Path $exeDef -Value $exeDefLines -Encoding Ascii
& $libexe /nologo "/def:$exeDef" "/out:$(Join-Path $TargetDir 'rustpython.lib')" /machine:x64
if ($LASTEXITCODE -ne 0) { throw "lib.exe failed for rustpython.lib" }

# 3. Forwarder shims + their import libs.
foreach ($dllName in @("python311", "python314", "python3")) {
    $def = Join-Path $TargetDir "$dllName.def"
    $lines = @("LIBRARY $dllName", "EXPORTS")
    foreach ($name in $exports) {
        $lines += "    $name = rustpython.exe.$name"
    }
    Set-Content -Path $def -Value $lines -Encoding Ascii

    # The forwarding DLL itself (no code: every export forwards to the exe).
    $outDll = Join-Path $TargetDir "$dllName.dll"
    $linkCmd = "link /nologo /dll /noentry /def:$def /out:$outDll /machine:x64 /LIBPATH:$TargetDir"
    $linkOut = Invoke-VsEnv "$linkCmd 2>&1"
    if ($LASTEXITCODE -ne 0) {
        $linkOut | Select-Object -Last 10
        throw "link.exe failed for $dllName"
    }
    Write-Host "created $outDll"

    # Import library so .pyds can be linked against the shim.
    $plainDef = Join-Path $TargetDir "$dllName-plain.def"
    $plainLines = @("LIBRARY $dllName", "EXPORTS")
    foreach ($name in $exports) {
        $plainLines += "    $name"
    }
    Set-Content -Path $plainDef -Value $plainLines -Encoding Ascii
    & $libexe /nologo "/def:$plainDef" "/out:$(Join-Path $TargetDir "$dllName.lib")" /machine:x64
    if ($LASTEXITCODE -ne 0) { throw "lib.exe failed for $dllName.lib" }
    Write-Host "created $(Join-Path $TargetDir "$dllName.lib")"
}

# 4. Sanity checks.
$check = & $dumpbin /EXPORTS (Join-Path $TargetDir "python311.dll") |
    Select-String "PyModule_Create|PyArg_ParseTuple"
Write-Host "sanity: python311.dll exports:"
$check | Select-Object -First 4
