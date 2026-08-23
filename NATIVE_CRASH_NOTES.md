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
