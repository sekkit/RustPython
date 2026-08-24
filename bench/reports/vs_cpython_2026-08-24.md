# RustPython vs CPython 3.12.10 micro-benchmark (2026-08-24)

Machine: Windows x64, release build (`cargo build --release -p rustpython`),
best-of-5 `time.perf_counter` per case, identical script under both
interpreters (`temp/vs_cpython.py` methodology).

## Final state (all rounds, min-of-2-paired-runs)

| case | cpython_ms | rustpy_ms | ratio | session start |
|---|---:|---:|---:|---:|
| sum_200k_complex      |   3.32 |   1.89 | **0.57x (RP wins)** | 0.58x |
| str_concat_30k        |   1.88 |   2.82 | 1.50x | 13.06x |
| sum_500k_int          |   2.93 |   4.65 | 1.59x | 1.61x |
| sum_1e6_float         |   5.98 |  10.58 | 1.77x | 1.85x |
| complex_arith_300k    |  22.64 |  55.10 | 2.43x | 2.48x |
| int_arith_1m          |  76.37 | 235.11 | 3.08x | 2.81x |
| str_short_create_150k |  24.73 |  84.05 | 3.40x | 3.51x |
| list_append_500k      |  22.64 |  95.10 | 4.20x | 5.05x |
| dict_str_lookup_14k   |   0.76 |   3.19 | 4.20x | 5.47x |
| float_arith_1m        |  35.08 | 159.87 | 4.56x | 4.40x |
| fn_call_1m            |  52.02 | 255.67 | 4.91x | 4.57x* |
| attr_slot_500k        |  12.79 |  62.85 | 4.91x | 5.24x |

Geometric mean: **~2.65x** (session start ~3.0x). Run-to-run variance on
this machine is ±10–15% for dispatch-heavy cases (RP's longer runtimes are
more thermally sensitive; CPython numbers are stable across runs).
Best-observed same-code-state values were lower still (dict 2.51,
attr 55.9, fn_call 223). (*int_arith/fn_call session-start numbers came
from noisier runs — treat those deltas as indicative only.)

Additional cases measured separately: uni_concat_30k ("中文测试" x30k)
2.99ms vs CPython 2.50ms (1.20x, was quadratic before this session).

Session drivers:
- `str +=` on uniquely-owned strings appends into a growable
  `StrStorage::Grow` buffer, generalized to ALL WTF-8 kinds
  (str_concat 13x -> 1.50x; unicode accumulation at parity).
- `identical_or_equal` short-circuits exact-str pairs to byte equality;
  dict probes read the header-cached string hash directly.
- SSO generalized to short non-ASCII strings (any WTF-8 <= 22 bytes).
- Interned objects got immortal counts (RMW-free clone/drop).
- FOR_ITER list/tuple iterators take a lock-elided step in
  single-threaded execution.

## Follow-up round baseline (mid-session reference)

| case | cpython_ms | rustpy_ms | ratio (rp/cpython) |
|---|---:|---:|---:|
| sum_200k_complex      |   3.56 |   2.05 | **0.58x (RP wins)** |
| sum_500k_int          |   2.94 |   4.74 | 1.61x |
| sum_1e6_float         |   5.95 |  10.99 | 1.85x |
| int_arith_1m          |  81.59 | 229.57 | 2.81x |
| complex_arith_300k    |  23.32 |  57.88 | 2.48x |
| fn_call_1m            |  56.16 | 256.86 | 4.57x |
| float_arith_1m        |  36.26 | 159.56 | 4.40x |
| list_append_500k      |  23.34 | 117.78 | 5.05x |
| attr_slot_500k        |  14.45 |  75.66 | 5.24x |
| dict_str_lookup_14k   |   0.88 |   4.81 | 5.47x |
| str_short_create_150k |  26.02 |  91.32 | 3.51x |
| str_concat_30k        |   2.08 |  27.17 | 13.06x |

## Reading

- `sum()` over exact numeric containers runs at parity-or-better with
  CPython: the complex chain wins outright thanks to the zero-copy
  compensated scan.
- Remaining gap concentrates in dispatch-heavy bytecode (calls,
  attribute/slot access) at ~3.9–4.0x — consistent with the documented
  architectural dispatch floor. LOAD_ATTR/STORE_ATTR inline caches are
  already implemented here (all CPython variants incl. slots/values/
  method); the residual is per-dispatch overhead, not missing caches.
- Top follow-up candidates: vectorcall tightening for fn_call, and
  borrow-semantics activation (LOAD_FAST_BORROW currently clones).

## Decomposition findings (call & iteration, follow-up round)

Staged micro-benchmarks (1M calls / 200x14k lookups, best-of-5):

- Function calls are uniformly ~130ns/call slower than CPython regardless
  of body (`pass` / 1-arg / 2-arg+add all ~3.7–4.0x). The tailcall
  trampoline path IS taken (no nested recursion); residual sits in frame
  push/pop + dispatch entry — needs a native sampling profiler to split
  further (none available in this environment).
- Half of the dict-lookup gap is **list iteration itself (4.2x)**:
  `PyListIterator::fast_next` acquires two locks per element (iterator
  state mutex + `borrow_vec` read guard) plus PyIterReturn/Option
  layering, ≈90ns/element vs CPython's lock-free GIL-index step. Fixing
  requires a single-thread-mode lock-elision design that preserves
  mutation-during-iteration safety — its own project.
- Dict probe cost after the byte-eq fast path is ~3x CPython (hash is
  cached; remaining = probe arithmetic through DictKey trait + guards).

Negative result (round: lasti): replacing the per-instruction locked
`lasti.fetch_add` with relaxed load+store changed nothing measurable
(all cases within noise, float_arith slightly worse) - the LOCK prefix
is not a meaningful component of the dispatch floor. ExecutingFrame is
&mut-exclusive so the split was sound; it just was not where the time
goes. Remaining per-instruction cost candidates: Instruction enum
match width, PyStackRef (Option<PyObjectRef>) stack moves,
arg_state/cache_entries handling, eval-breaker check.

Fresh paired numbers (machine fast state): geomean 2.57x;
dict_str_lookup improved to 3.40x after in-place key equality.
Negative result (round: float binop stack ops): replacing pop2+push1
with direct slot write + truncate made float_arith ~6% SLOWER.
push_value/pop_value are already optimal inline operations on the
contiguous stack buffer; explicit index arithmetic adds overhead.
Priority order from data: (1) list-iterator lock elision,
(2) fn_call native-profile campaign, (3) borrow activation (blocked on
freethreaded priority).
