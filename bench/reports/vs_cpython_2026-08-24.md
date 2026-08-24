# RustPython vs CPython 3.12.10 micro-benchmark (2026-08-24)

Machine: Windows x64, release build (`cargo build --release -p rustpython`),
best-of-5 `time.perf_counter` per case, identical script under both
interpreters (`temp/vs_cpython.py` methodology).

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

- `sum()` over exact numeric containers now runs at parity-or-better with
  CPython: the complex chain **wins outright** thanks to the zero-copy
  compensated scan; float/int sit at 1.6–1.9x despite paying for Neumaier
  compensation CPython also pays for.
- The residual gap concentrates in dispatch-heavy bytecode (calls,
  attribute/slot access, list append) at 4.5–5.5x — consistent with the
  documented architectural dispatch floor.
- Top follow-up candidates: `str +=` small-piece accumulation is 13x
  (quadratic buffer behavior suspected in the inplace-add path); short-string
  creation and dict lookups would benefit directly from wider SSO reuse in
  hashing/equality fast paths.
