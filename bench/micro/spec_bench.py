# Specialization cache value: CallPyExactArgs monomorphic vs megamorphic
#
# RustPython adaptively specializes Instruction::Call into
# Instruction::CallPyExactArgs when the callable is a PyFunction with exactly
# `arg_count == effective_nargs` (no varargs/varkw/kwonly, frame.rs ~line 6025).
# The handler consults an inline func_version cache:
#   * HIT  (cached func_version == actual PyFunction version)
#          -> fast path: pop args into CallArgBuffer, invoke_exact_args_slots
#   * MISS (version differs, e.g. different function each call)
#          -> execute_call_vectorcall (generic path)
#
# Monomorphic: same function object called every iteration  -> version cache HIT.
# Megamorphic: K distinct functions rotated                   -> version cache MISS.
#
# ns/call delta (miss - hit) = value of one specialization-cache hit
# on the CallPyExactArgs instruction.
import time

N = 4_000_000
K = 16  # megamorphic working set size

# Generate K single-arg functions (arg_count=1, no defaults -> eligible for
# CallPyExactArgs). Distinct code objects -> distinct func_versions.
def _make_funcs(k):
    g = {}
    for j in range(k):
        exec(f"def f{j}(x):\n    return x + {j}", g)
    return [g[f"f{j}"] for j in range(k)]


FS1 = _make_funcs(1)
FSK = _make_funcs(K)


# Identical loop shape for mono and mega: `fs[i & MASK](i)`, differing only
# in the function-object working set (1 vs K). The `i & MASK` index keeps the
# bytecode sequence byte-for-byte the same, so the ns/call delta isolates the
# CallPyExactArgs cache hit/miss, not loop arithmetic.
def bench_mono(iters):
    fs = FS1
    s = 0
    for i in range(iters):
        s += fs[i & 0](i)
    return s


def bench_mega(iters):
    fs = FSK
    s = 0
    for i in range(iters):
        s += fs[i & 15](i)
    return s


def timeit(fn, iters, reps=5):
    fn(100_000)  # warmup
    best = float("inf")
    for _ in range(reps):
        t0 = time.perf_counter()
        r = fn(iters)
        t1 = time.perf_counter()
        best = min(best, (t1 - t0) / iters * 1e9)
    return best, r


def main():
    ns_mono, r1 = timeit(bench_mono, N)
    print(f"monomorphic (cache HIT):   {ns_mono:8.1f} ns/call  s={r1}")

    ns_mega, r2 = timeit(bench_mega, N)
    print(f"megamorphic (cache MISS):  {ns_mega:8.1f} ns/call  s={r2}")

    delta = ns_mega - ns_mono
    print(f"miss - hit delta:          {delta:8.1f} ns/call  ({(ns_mega / ns_mono):.2f}x)")


if __name__ == "__main__":
    main()
