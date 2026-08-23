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
DECISIVE: enter prints, map_len never does => AV inside Mutex::lock()
itself, on a HEAP-allocated Mutex (OnceLock-relocated). Heap corruption
from elsewhere. Unified theory: allocator mismatch - native objects
taking the is_foreign_object true branch (vtable heuristic false
positive) get libc::free()d while allocated by Rust's global allocator
=> heap corruption => delayed AV at next allocation/lock.
FIX NEXT: restrict is_foreign_object to registry-only; drop heuristic.
Update: registry-only is_foreign_object (heuristic removed as unsafe on
free path) - crash unchanged. ALL environmental/allocator theories now
exhausted. Remaining suspect: re_compile writes through a pointer we
sized with tp_basicsize from the STUB, but the stub's basicsize field
may be stale/wrong for the extension type - verify by dumping
tp_basicsize read in _PyObject_New vs PatternObject real size, and
check whether PyType_Ready (currently no-op!) is expected to finalize
basicsize. CPython extensions rely on PyType_Ready having run.
Update round 276: PyType_Ready suspect WEAKENED - extensions set
tp_basicsize themselves in their static type structs before calling
PyType_Ready (no-op safe for well-formed extensions). Sizing via
_PyObject_New should be correct.
Remaining sharpest suspect: PyArg_ParseTuple conversion writing caller
slots. If our VaSlots sizing/stride mismatches MSVC va_list layout for
re_compile's format string, one converted value lands in the wrong
slot => later NULL deref INSIDE re_compile. Next: instrument
parse_format success path to dump slot values+addresses under
RUSTPYTHON_TRACE for the compile call.
Update round 277: VaSlots stride theory WEAKENED - getargs.c C shim
(compiled with real MSVC) converts va_list to the slots array before
rp_va_parse_tuple sees it; layout handling is native C, inherently
MSVC-correct.
Next diagnostic: log format string + nslots in rp_va_parse_tuple under
RUSTPYTHON_TRACE during compile call; compare against expected 11-arg
signature; also check whether _regex.compile def even uses KEYWORDS
(flag arm coverage from fn='' earlier suggests plain VARARGS).
DECISIVE: fmt="OnOOOOOnOnn:re_compile" nslots=11 nargs=11 - parse
SUCCEEDS. Crash is post-parse inside re_compile body: some capi call it
makes receives NULL first arg (rcx=0). Parse-converted slots are valid
raw ptrs (O) or ints (n). Suspects within re_compile flow:
PyDict_GetItem/Next on the dicts, PyUnicode_* on pattern 'hello',
PyList ops on code list, or PyObject_New'd PatternObject field init
reading a NULL it got from one of those. Next: entry-log PyDict_*
family + PyUnicode_AsUTF8AndSize callers under window; first NULL-arg
log before VEH names the callee.
Update round 280: DICT-GET never fires - dict family eliminated.
Parse succeeded with correct 11 args (no self-shift). PRIME THEORY NOW:
re_compile uses INLINED CPython accessor macros (PyUnicode_GET_LENGTH /
PyUnicode_READ_CHAR etc.) compiled against CPython str layout - reads
ob_size/fixed-data at offsets our PyStr layout places differently,
producing NULL/garbage => AV read [NULL+0x10]. This is the fundamental
str-object-layout gap: fix requires our str stubs/objects to expose
CPython-compatible fields at +0x10 for length (and data pointer), or
exporting non-inline PyUnicode_* replacements for every macro regex
uses. Verify next: dump bytes at pattern_obj+0x10 from VEH registers -
rdx holds a heap ptr each crash; check whether it equals pattern obj.
MILESTONE: byte-level str layout comparison captured.
CPython 'hello world': [+10]=len(11), [+28..]='hello world' inline.
RustPython:           [+10]=hash(-1!), [+18]=data-box ptr, len at +20/+28,
data on heap elsewhere. Inlined CPython macros read len@+10 => get our
hash sentinel; read data@+28 => get pointers. THIS is the regex AV root:
str-object-layout divergence, now proven with side-by-side dumps.
Fix path: PyStr must store length at +16 and compact ascii data at +40
(CPythons compact layout) for all strings, or extensions see garbage.
Full dumps preserved in git history of this note file addition.
LAYOUT SCAN: 'hello world' RustPython qwords: [+10]=-1(hash), [+18]=box,
[+20]=0xb, [+28]=0xb => length sits at +32/+40 ABSOLUTE (payload starts
+16 AFTER PyInner header; pyclass ordering puts hash first or GC prefix
shifts). CPython expects length at absolute +16. Delta = +16..24 shift.
FIX: static-assert offset_of!(PyStr-ish view from obj base, length)==16;
reorder payload (length first) and/or account PyInner size so absolute
offset matches; then inline-data question remains for READ_CHAR.
