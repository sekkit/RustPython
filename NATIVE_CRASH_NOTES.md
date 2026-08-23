# Native-crash investigation state (regex _regex PYD)

Fault: AV read [0+0x10] inside resolve_dynamic_stub_addr+0x127 at
TYPE_STUB_CACHE.lock() on the FIRST call - Mutex static corrupted
before use, deterministic across heap layouts.

Audited clean:
- objectstatics::fill_type_stub: max write words[21] (offset 168) <
  256-byte ObjectHeaderCopy stubs. No overflow.
- decode_fsdefault_and_size: NULL-safe.
- NULL-contract sweep: zero capi NULL-without-exception results.

Remaining suspects:
1. init_exception_statics writes (crates/capi/src/pyerrors.rs)
2. Dual-image statics: capi code exists in BOTH rustpython.exe AND
   python314.dll relay - each has its OWN TYPE_STUB_CACHE. Verify which
   image the faulting IP belongs to per MAP base vs relay base.
3. ensure_object_statics header-copy path.

Next step: print module name for IP using GetModuleHandleExW and
compare against BOTH image bases; then audit pyerrors.rs static writes.
Update: real-args repro captured (JSON round-trip of actual
_regex.compile arguments from working CPython 3.14). Crash STILL
occurs - proving the bug is not invalid-input-related. New backtrace
frame: malachite-nz from_power_of_2_digits inside rustpython_capi -
bigint conversion path involved when extracting large opcode ints from
the code list. Next: probe PyLong_AsUnsignedLong with the real large
opcode values (e.g. >2^32) and check our BigInt->c_ulong conversion.
CONFIRMED via GetProcAddress: rustpythonapi.dll contains REAL capi code
(Py_TYPE @ +offset in its range); python314.dll GetProcAddress returns
NULL for the same name (forwarder-stub export). The exe has a third,
statically-linked copy. => Dual (actually tri-image) statics CONFIRMED:
PYD-side capi calls run rustpythonapi.dll copies; interpreter-side run
exe copies. Two independent TYPE_STUB_CACHEs exist.
Update: shims regenerated (make_python_dll_shims.ps1) - crash UNCHANGED.
Function thunks self-heal via rustpythonapi_init(GetModuleHandle(NULL)),
so dual-image static theory also eliminated: all capi calls land in exe
code with exe statics. Eliminated so far: NULL-contract, .data stomp,
bigint conv, boundary probes, NULL-self, stale shims, dual images.

Remaining: fault truly inside resolve_dynamic_stub_addr LTO range per
MAP+PDB agreement. Next session: capture full unfiltered STUBCACHE
trace lines around crash (earlier greps filtered them); determine
whether enter/map_len print order places fault inside lock() vs iter();
then dump raw HashMap internals (table ptr, ctrl) from the VEH using
the known &static address.
