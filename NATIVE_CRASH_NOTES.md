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
Round 285 replay: crash persists - confirms inline-data@+40 is THE last
blocker. Length+state parity insufficient; regex computes
PyUnicode_DATA = obj+40 for compact strings and reads our heap-boxed
storage there.
IMPLEMENTATION PLAN (multi-round):
1. PyStr layout: {length@p+0, hash@p+8, state@p+16} payload = 24B;
   inline WTF-8 bytes begin abs+40. Allocation: 40+len+1 via oversized
   PyInner alloc (reuse extra-bytes hook planned for _PyObject_New).
2. StrData removed from object; kind lives in state bits; bytes accessed
   via raw slice from data ptr. All ~36 call sites in str.rs migrate to
   slice-based ops. Interning/latin1 fast paths reworked.
3. Hash computed lazily into hash field as today.
Estimate: rounds 286-292 for core migration, then test-suite shakedown.
Step 2 scoping (this round): identified exact wiring points for inline
bytes without breaking readers - DUAL STORAGE transition:
1. Context::new_str / intern paths know byte_len pre-allocation; route
   into_ref -> new_ref -> new_with_extra(extra = len+1).
2. After alloc, memcpy WTF-8 into tail (abs+40) and record byte_len.
3. Keep existing StrData field temporarily (readers unchanged); regex
   macros read the tail directly = real data => AV resolved NOW.
4. Later rounds migrate readers to tail slices, then drop StrData.
Blocker found while scoping: new_ref signature lacks extra passthrough;
needs PyPayload::into_ref_with_extra or a Context::new_str_inline that
bypasses freelist. Next session implements that bypass (est 1 round).
Round 289 scoping: data found at offset +88 from obj base (CPython
expects +40). sizeof(PyStr)=72 (repr(C), with StrData=48B inside).
THE FIX: remove `data: StrData` from PyStr => sizeof(PyStr)=24 =>
tail lands at abs+40 exactly. Steps:
1. PyStr struct: {length: isize, hash: AtomicI, state: u32} = 24B
2. Store byte_len in length field (ASCII strings: char==byte count)
   For non-ASCII: store byte count too; char count derived on demand
3. as_wtf8()/data() return slice from raw self-ptr+40, len from length
4. ~36 call sites already route through data() - most flip transparently
5. concat_in_place (*payload).data write needs rework for tail mutation
6. From<StrData> impls write bytes into tail during new_ref_with_extra

Estimate: 1-2 rounds of intensive editing + full test shakedown.
THIS IS THE FINAL STRUCTURAL CHANGE for regex compatibility.
IMPLEMENTATION PLAN UPDATE (header-only PyStr refactor):
The struct removal is trivial (done+reverted); the hard part is the
36-call-site migration. data() currently returns &StrData (owned field).
Without the field, it must return a borrowed view. Two approaches:
A) StrDataRef<'a> wrapper struct { bytes:&[u8], kind:StrKind } with
   Deref<Target=StrDataLike> trait so callers mostly work unchanged.
   Requires a StrDataLike trait abstracting owned vs borrowed access.
B) Change data() to return StrData (by value, constructing from raw
   bytes each call). Simpler but allocates per call - unacceptable for
   hot paths unless we use SmallVec-like inline storage.
Approach A is correct. Estimated effort: define trait + impl on StrData
and new StrDataRef, update ~5 signature sites, bulk-fix remaining via
Deref coercion. 1-2 focused sessions.
SYMBOL SCAN FINDING: 87/90 symbols resolve through python314.dll.
PyUnicode_ToLowercase flagged as potential name mismatch - we export
_PyUnicode_ToLowercase (with underscore). If PYD imports without
underscore prefix, it resolves via forwarder chain differently.
VERIFIED: PyUnicode_ToLowercase NOT FOUND on python314.dll.
_PyUnicode_ToLowercase IS exported. However, PYD loads successfully =>
all imports resolved => either PYD does not import this name (string
from debug info/data section) or uses a different mechanism. Likely
false positive from binary string scanning.
HEADER-ONLY PyStr REFACTOR - DESIGN BLOCKER + SOLUTION (round 1):
BLOCKER: PyStr is both an allocated payload (reads tail at +40) and a
VALUE passed to Context::new_str(impl Into<PyStr>). A header-only
(24B) PyStr value cannot carry bytes to allocation time, so new_str
cannot copy them into the tail.
SOLUTION: change Context::new_str to take `impl Into<StrData>` (or a
new PyStrData<'_> carrier). Bytes flow to allocation, tail written
there; PyStr values only exist for allocated objects (data() reads
tail). All From<T> for PyStr impls already go through StrData, so
callers change minimally (new_str(x) where x: Into<StrData> still
accepts &str/String/Wtf8/char/&[u8]) via existing From impls.
LANDED: StrDataRef<'a> borrowed view {bytes, kind, char_len} with all
14 accessors (as_wtf8/as_str/as_ascii/kind/as_str_kind/len/is_empty/
char_len/char_index_to_byte/char_range_to_bytes/byte_to_char_index/
nth_char/clone_to_data) - compiles in rustpython-common.
NEXT: (a) add From<StrData> to carry through new_str; (b) switch
new_str signature; (c) strip PyStr to 24B; (d) migrate 36 call sites.
## Round 2 Findings: Atomic Refcount Cost + Cumulative Performance Summary

### Experiment: threading atomics cost ~10%
Built --no-default-features (no threading => non-atomic refcounts via
Radium) and compared: hot_loop 0.377s -> 0.348s, all micro-ops ~10%
faster. This is the free-threading tax: RustPython default-builds with
threading so every incref/decref is lock xadd. CPython free-threaded
pays the same class of cost (~30% vs GIL build).

Options: (a) accept 10%, (b) biased/deferred refcounting (CPython
3.13t approach, big project), (c) make threading opt-in.

### Cumulative performance gains this optimization series
| Benchmark | Baseline | Now | Speedup | Gap vs CPython |
|---|---|---|---|---|
| hot_loop 2M | 0.449s | 0.377s | 1.19x | 3.7x -> 3.1x |
| for_loop_1k | 44.9us | 25.0us | **1.8x** | 4.5x -> 3.0x |
| str_split 22KB | 680us | 259us | **2.6x** | 6.3x -> 2.4x |
| list_compr_1k | 162us | 118us | 1.4x | 4.2x -> 3.1x |
| sum(list 50k) | 3554us | 155us* | **22.9x** | 21x -> 4.7x |
(*with-threading build, fast path)
Plus no-threading variant reaches for_loop 22.5us / gap 2.7x.

### Optimizations landed
1. lazy f_lineno (per-instruction bookkeeping removed)
2. mimalloc global allocator
3. sum() exact-int fast path (i128 accumulator)
4. inline buffer at CPython compact-data offset +40
5. PyType_Ready sets ob_type; PyModule_GetState state buffer;
   _PyObject_GC_New GC-head space (correctness fixes enabling PYDs)

### Next highest-value targets
1. Superinstructions: fuse FOR_ITER+STORE_FAST etc (Phase 1 roadmap)
2. Biased refcounting to reclaim the threading tax under default build
3. LOAD_ATTR cache hit-rate audit (specialize_load_attr exists; verify
   it fires on hot paths via counters dump)
## Round 3: Specialization Verification + Clean Baseline

VERIFIED: PEP 659 specialization IS firing correctly on hot paths.
- _co_code_adaptive shows (130,0) = BinaryOpAddInt after warmup
- dis co_code shows ORIGINAL opcodes by design (original_bytes());
  live state requires _co_code_adaptive — earlier "not specializing"
  readings were an artifact of reading the wrong view.
- Superinstructions also active: LOAD_FAST_BORROW_LOAD_FAST_BORROW (87)

Clean baseline after removing debug instrumentation:
- hot_loop(2M): RustPython 0.380s vs CPython 0.116s = 3.3x
- All suites green: test_str/test_builtin/test_re

Optimization series total (from session start):
- hot_loop 0.449 -> 0.380s = 15% faster (3.7x -> 3.3x vs CPython)
- for_loop_1k 44.9 -> 25.0us = 1.8x (4.5x -> 3.0x)
- str_split 680 -> 259us = 2.6x (6.3x -> 2.4x)
- sum(list) 3554 -> 155us = 22.9x (21x -> 4.7x)
## Round 8: PyStr freelist experiment — REVERTED (hangs)

Adding MAX_FREELIST=400/HAS_FREELIST to PyStr compiled fine but caused
test_str to HANG. Likely cause: strings are heavily interned/shared;
freelist reuse races with intern-table references or the GC traverse
path expects stable object identity for str payloads. Unlike int/float
(immutable value cells, no interning), strings participate in
interning + weakref + hash caching lifetimes that the generic freelist
does not account for.

Conclusion: string allocation speedup must come from elsewhere:
- Context::new_str fast paths (already partially: dual-storage write)
- Intern table hit-rate improvements
- Possibly a size-classed small-string arena separate from freelist
## Performance Landscape After Optimization Series (updated)

### Operations now BEATING CPython 3.14
| Operation | RustPython | CPython | Advantage |
|---|---|---|---|
| for_loop_range_1k (sum(range)) | 0.67us | 7.5us | **11.2x faster** (O(1) formula) |
| list_sum (10k ints) | 16.9us | 29.2us | **1.7x faster** |
| str_count (22KB) | 3.5us | 7.1us | **2.0x faster** |
| str_replace (22KB) | 6.9us | 8.9us | **1.3x faster** |
| bigint_add | 0.64us | 0.41us | 1.6x gap only |

### At parity
str_upper 1.3x, tuple_in 2.1x, dict_keys_list 2.4x

### Remaining uniform floor: ~6-8x on micro-ops
int_add 5.7x, float_add 8x, fn_call 6.8x, method_call 8x,
dict_getitem 6.2x, list_getitem 6.8x, isinstance 7.2x, str ops 6.5-8.5x.
This is VM bytecode dispatch + calling-convention overhead common to ALL
operations — closing it requires:
1. Superinstruction fusion (Phase 1 roadmap)
2. Tail-call dispatch via nightly become (Phase 4)
3. Reduced PyRef atomic traffic on stack push/pop (biased refcounting)

Iteration-heavy composites improved dramatically: list_compr 3.1x (was 4.2),
list_contains 2.6x (was 3.2), tuple_in 2.1x (was 2.3), str_split 2.5x
(was 6.3 pre-mimalloc).
## Round: fetch_add lasti — 19% dispatch win

lasti was an atomic field touched 3-4x per instruction (load idx,
closure increment load+store, reload lasti_before, cache-skip store).
Replaced with single `fetch_add(1, Relaxed)` returning the executing
index; cache-skip now writes idx+1+caches directly.

hot_loop(2M): 0.380 -> 0.308s = 19% faster. Series cumulative:
0.449 -> 0.308s = 31% faster (gap 3.7x -> 2.9x vs CPython).

Remaining per-instruction atomic ops after this: read_op (Acquire),
read_arg (Relaxed), eval_breaker (Relaxed) = 3 loads, all necessary
for cross-thread quickening/signal semantics.
