# RustPython vs CPython 3.12.10 micro-benchmark (2026-08-24)

Machine: Windows x64, release build (`cargo build --release -p rustpython`),
best-of-5 `time.perf_counter` per case, identical script under both
interpreters (`temp/vs_cpython.py` methodology).

## After the concat/eq follow-up round

| case | cpython_ms | rustpy_ms | ratio | was |
|---|---:|---:|---:|---:|
| sum_200k_complex      |   3.56 |   1.76 | **0.49x (RP wins)** | 0.58x |
| str_concat_30k        |   2.08 |   2.65 | 1.27x | 13.06x |
| sum_500k_int          |   2.94 |   4.20 | 1.43x | 1.61x |
| sum_1e6_float         |   5.95 |   9.79 | 1.65x | 1.85x |
| int_arith_1m          |  81.59 | 207.90 | 2.55x | 2.81x |
| complex_arith_300k    |  23.32 |  50.29 | 2.16x | 2.48x |
| str_short_create_150k |  26.02 |  74.95 | 2.88x | 3.51x |
| float_arith_1m        |  36.26 | 139.58 | 3.85x | 4.40x |
| fn_call_1m            |  56.16 | 223.17 | 3.97x | 4.57x |
| list_append_500k      |  23.34 |  90.54 | 3.88x | 5.05x |
| attr_slot_500k        |  14.45 |  55.88 | 3.87x | 5.24x |
| dict_str_lookup_14k   |   0.88 |   2.51 | 2.85x | 5.47x |

Geometric mean of ratios moved from ~3.0x to ~2.4x.

Round drivers:
- `str +=` on uniquely-owned ASCII strings appends into a growable
  `StrStorage::Grow` buffer (13x -> 1.27x).
- `identical_or_equal` short-circuits exact-str pairs to byte equality
  and dict probes read the header-cached string hash directly; the fast
  path also lifts comparison-heavy generic workloads.

## Original baseline (before this round)

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
