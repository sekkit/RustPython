# build_test_extensions.ps1
#
# Builds CPython's extension-module test suite (test_importlib/extension) as
# real .pyd files linked against the RustPython C-API shim (python314.dll).
#
# Sources come from the CPython 3.14.x release tarball:
#   Modules/_testsinglephase.c  -> Lib\_testsinglephase.cp314-win_amd64.pyd
#   Modules/_testmultiphase.c   -> Lib\_testmultiphase.cp314-win_amd64.pyd
# (the multi-phase file exports all the *_bad_slot_*, *_nonmodule, *_exec_*,
#  *_export_* and non-ASCII init functions from the one DLL)
#
# The modules must be findable as top-level modules on sys.path, and the
# FileFinder only looks for <name> + the SOABI suffix (see
# _imp.extension_suffixes / sysconfig SOABI), so the built files land in Lib\
# under their SOABI names.
#
# Usage:  powershell -ExecutionPolicy Bypass -File bench\build_test_extensions.ps1
#
# Requires: MSVC toolchain, `cargo build --release` (capi feature), and
# `bench\make_python_dll_shims.ps1` run so python314.lib exists in target\release.

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

$ver = "3.14.7"
$labs = Join-Path $root "bench\labs"
$srcDir = Join-Path $labs "Python-$ver"
$incDir = Join-Path $labs "cp314inc"
$extSrc = Join-Path $labs "extsrc"
$target = Join-Path $root "target\release"
$soabi = "cp314-win_amd64"

# 1. Fetch the CPython source tarball if needed.
$tgz = Join-Path $labs "cp314-src.tgz"
if (-not (Test-Path (Join-Path $srcDir "Include\Python.h"))) {
    if (-not (Test-Path $tgz)) {
        Write-Host "downloading CPython $ver source..."
        Invoke-WebRequest -Uri "https://www.python.org/ftp/python/$ver/Python-$ver.tgz" -OutFile $tgz
    }
    tar -xzf $tgz -C $labs
}

# 2. Assemble the include directory: public + internal headers, pyconfig.h,
#    and the clinic-generated files.
if (-not (Test-Path (Join-Path $incDir "Python.h"))) {
    New-Item -ItemType Directory -Force -Path $incDir | Out-Null
    Copy-Item (Join-Path $srcDir "Include\*") $incDir -Recurse -Force
    Copy-Item (Join-Path $srcDir "PC\pyconfig.h") (Join-Path $incDir "pyconfig.h") -Force
    New-Item -ItemType Directory -Force -Path (Join-Path $incDir "clinic") | Out-Null
    Copy-Item (Join-Path $srcDir "Modules\clinic\_testmultiphase.c.h") (Join-Path $incDir "clinic\") -Force
    Write-Host "assembled include dir at $incDir"
}

# 3. Fetch the test module sources if needed.
if (-not (Test-Path (Join-Path $extSrc "_testmultiphase.c"))) {
    New-Item -ItemType Directory -Force -Path $extSrc | Out-Null
    Copy-Item (Join-Path $srcDir "Modules\_testmultiphase.c") $extSrc -Force
    Copy-Item (Join-Path $srcDir "Modules\_testsinglephase.c") $extSrc -Force
}

# 4. Locate MSVC.
$vsRoot = "C:\Program Files\Microsoft Visual Studio"
$vcvars = Get-ChildItem $vsRoot -Directory -ErrorAction SilentlyContinue |
    ForEach-Object { Get-ChildItem $_.FullName -Directory -ErrorAction SilentlyContinue } |
    ForEach-Object { Join-Path $_.FullName "VC\Auxiliary\Build\vcvars64.bat" } |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $vcvars) { Write-Error "vcvars64.bat not found" }
function Invoke-VsEnv([string]$command) {
    cmd /c "call `"$vcvars`" >nul 2>&1 && $command"
}
$shimLib = Join-Path $target "python314.lib"
if (-not (Test-Path $shimLib)) {
    Write-Error "python311.lib not found in $target - run bench\make_python_dll_shims.ps1 first"
}

# 5. Compile.
foreach ($mod in @("_testsinglephase", "_testmultiphase")) {
    $src = Join-Path $extSrc "$mod.c"
    $out = Join-Path $root "Lib\$mod.$soabi.pyd"
    $cmd = "cl /nologo /LD /O2 /I `"$incDir`" /I `"$incDir\internal`" /I `"$extSrc`" `"$src`" /link /OUT:`"$out`" /LIBPATH:`"$target`" 2>&1"
    $outLines = Invoke-VsEnv $cmd
    $outLines | Select-Object -Last 6
    if (-not (Test-Path $out)) {
        Write-Error "failed to build $mod.$soabi.pyd (see compiler output above)"
    }
    Write-Host "built $out"
}
