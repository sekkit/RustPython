# Binary wheel install demo (pip -> site-packages -> import)

Proves the full C-extension deployment chain on RustPython:

1. `spam.c` is compiled to `spam.cp314-win_amd64.pyd` (SOABI suffix) against the
   CPython 3.14 headers (`bench\labs\cp314inc`) and the `python314` shim
   (`target\release\python314.lib`, from `bench\make_python_dll_shims.ps1`).
2. `make_spam_wheel.py` (run with RustPython) assembles
   `spam-1.0-rustpython314-cp314-win_amd64.whl` with correct RECORD hashes.
3. `pip install` accepts the wheel because `sysconfig.SOABI == "cp314-win_amd64"`
   and the interpreter tag derives from `sys.implementation.name`:
   `rustpython314-cp314-win_amd64` is in pip's supported tag set.
4. `import spam` finds `spam.cp314-win_amd64.pyd` in site-packages through the
   `_imp.extension_suffixes()` SOABI suffix.

Why the tag is `rustpython314-cp314-win_amd64`:
- interpreter part = `rustpython` + version digits (packaging.tags uses
  `sys.implementation.name`; CPython would be `cp314`);
- ABI part = `cp314` (from SOABI) — extensions link the python314 shim;
- the file inside the wheel keeps the SOABI name (`spam.cp314-win_amd64.pyd`),
  exactly like CPython wheels.

Real CPython 3.14 wheels (`cp314-cp314-win_amd64`) are rejected by pip, which
is deliberate: only extensions built against the RustPython shim ABI load.

## Commands

```powershell
# 1. build the extension (MSVC x64 env required)
cl /nologo /LD /O2 /I bench\labs\cp314inc /I bench\labs\cp314inc\internal `
    bench\wheel_demo\spam.c /link /OUT:bench\wheel_demo\spam.cp314-win_amd64.pyd `
    /LIBPATH:target\release

# 2. build the wheel (with RustPython)
$env:RUSTPYTHONPATH="$PWD\Lib;$PWD\bench\labs"
.\target\release\rustpython.exe bench\wheel_demo\make_spam_wheel.py

# 3. install and import
.\target\release\rustpython.exe -m pip install --no-index --no-deps `
    bench\wheel_demo\spam-1.0-rustpython314-cp314-win_amd64.whl
.\target\release\rustpython.exe -c "import spam; print(spam.hello(), spam.add(20, 22))"
```

Requires: `cargo build --release`, `bench\make_python_dll_shims.ps1`,
`bench\build_test_extensions.ps1` (for cp314inc), and `-m ensurepip` once.
