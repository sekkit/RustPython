# Dispatch-cost microbenchmark: RustPython vs CPython
# Measures ns per bytecode instruction executed.
#
# Methodology for the "dispatch floor":
#   Straight-line bodies with NO branches/loops, so the executed instruction
#   sequence is exactly `instr_per_call` and ns/bytecode is unambiguous.
#   instr_per_call is computed from `dis` (count of real opcodes, excluding
#   CACHE / EXTENDED_ARG overhead so it reflects dispatchable opcodes).
#
# Methodology for loops:
#   ns/iter reported; per-iteration bytecode count transcribed from dis()
#   output so ns/bytecode can be derived.
#
# Run:
#   python bench/micro/dispatch_bench.py            (CPython)
#   $env:RUSTPYTHONPATH=... ; target/release/rustpython.exe bench/micro/dispatch_bench.py
import dis
import time

# ---------------- counting helpers ----------------

def _real_ops(fn):
    """Return list of (offset, opname) excluding CACHE/EXTENDED_ARG/Pseudo."""
    out = []
    for ins in dis.get_instructions(fn.__code__):
        n = ins.opname
        if n in ("CACHE", "EXTENDED_ARG") or "Pseudo" in n:
            continue
        out.append((ins.offset, n))
    return out


def _loop_iters_instr(fn):
    """Count REAL instructions executed per loop iteration.

    Strategy: find the only backward jump (the loop's JUMP_BACKWARD), take its
    target offset as loop header, count real instructions in
    [header .. jump] inclusive. Works for while- and for-loops on both
    CPython 3.12 and RustPython 3.14 opcode sets; first-iteration setup and
    the final exit test are amortized away by the large iteration count.
    """
    ops = _real_ops(fn)
    names = [n for _, n in ops]
    jumps = [i for i, n in enumerate(names) if "JUMP_BACKWARD" in n]
    if len(jumps) != 1:
        raise RuntimeError(f"expected 1 backward jump, got {jumps}: {names}")
    j = jumps[0]
    target_off = None
    for ins in dis.get_instructions(fn.__code__):
        if ins.opname == names[j] and ins.argval is not None:
            target_off = ins.argval or ins.offset
            break
    if target_off is None:
        raise RuntimeError("could not resolve back-jump target")
    # count real instructions with header <= offset <= jump offset
    jump_off = ops[j][0]
    cnt = sum(1 for off, _ in ops if target_off <= off <= jump_off)
    return cnt


def make_straight(name, body_src, args="x"):
    """Build a function by exec-ing source, returns (fn, op_names)."""
    src = [f"def {name}({args}):"]
    src.extend(body_src)
    g = {}
    exec("\n".join(src), g)
    fn = g[name]
    return fn, _real_ops(fn)


def bench_straight(name, fn, ops, runs=200_000, arg=1, reps=5):
    """Time `runs` calls of a straight-line function; ns/bytecode (min of reps)."""
    # warmup
    for _ in range(2_000):
        fn(arg)
    best_call = float("inf")
    for _ in range(reps):
        t0 = time.perf_counter()
        for _ in range(runs):
            fn(arg)
        t1 = time.perf_counter()
        best_call = min(best_call, (t1 - t0) / runs * 1e9)
    per_bc = best_call / max(len(ops), 1)
    print(f"{name}: {best_call:8.1f} ns/call  {len(ops):4d} instr  {per_bc:7.2f} ns/bytecode")
    return per_bc


def bench_loop(name, fn, iters, per_iter_instr, reps=5):
    """Time a loop function; ns/iter and ns/bytecode (min of reps)."""
    best_iter = float("inf")
    for _ in range(reps):
        t0 = time.perf_counter()
        r = fn(iters)
        t1 = time.perf_counter()
        best_iter = min(best_iter, (t1 - t0) / iters * 1e9)
    per_bc = best_iter / max(per_iter_instr, 1)
    print(f"{name}: {best_iter:8.1f} ns/iter ({per_iter_instr:2d} instr/iter) {per_bc:7.2f} ns/bytecode  r={r}")
    return per_bc


def main():
    print("== straight-line dispatch floor (ns/bytecode) ==")

    # 1) pure LOAD_FAST + STORE_FAST pairs (x=x => LOAD_FAST; STORE_FAST) 2 instr/rep
    body = ["    x = x"] * 200
    fn, ops = make_straight("f_load_store", body)
    bench_straight("load_store_x200", fn, ops, runs=100_000)

    # 2) LOAD_CONST + STORE_FAST (y = 1) 2 instr/rep
    body = ["    y = 1"] * 200
    fn, ops = make_straight("f_const_store", body)
    bench_straight("const_store_x200", fn, ops, runs=100_000)

    # 3) LOAD_FAST + LOAD_FAST + BINARY_OP + STORE_FAST (x = x + x) 4 instr/rep
    body = ["    x = x + x"] * 100
    fn, ops = make_straight("f_add", body)
    bench_straight("load_add_store_x100", fn, ops, runs=100_000)

    # 4) PURE dispatch: identical trivial op repeated (x+y toggled store) --
    #    largest block, most representative of "just dispatch".
    body = ["    x = x"] * 1000
    fn, ops = make_straight("f_big", body)
    bench_straight("load_store_x1000", fn, ops, runs=20_000)

    print()
    print("== loop-based (ns/iter and ns/bytecode) ==")

    # while loop: per iter = LOAD_FAST i, LOAD_FAST n, COMPARE_OP, POP_JUMP_IF_FALSE, LOAD_FAST i, LOAD_CONST 1, BINARY_OP, STORE_FAST i, JUMP_BACKWARD = 9 instr
    def while_counter(n):
        i = 0
        while i < n:
            i += 1
        return i

    wc = _loop_iters_instr(while_counter)
    print("  while_counter instr/iter:", wc)
    bench_loop("while_counter", while_counter, 3_000_000, per_iter_instr=wc)

    # for-loop with body: per iter = STORE_FAST i, body, JUMP_BACKWARD
    # (counted automatically from dis)
    def for_sum(n):
        s = 0
        for i in range(n):
            s += 1
        return s

    fs = _loop_iters_instr(for_sum)
    print("  for_sum instr/iter:", fs)
    bench_loop("for_sum", for_sum, 1_000_000, per_iter_instr=fs)

    # empty call: CALL in a loop
    def empty():
        pass

    def call_empty_loop(n):
        s = 0
        for _ in range(n):
            empty()
            s += 1
        return s

    ce = _loop_iters_instr(call_empty_loop)
    print("  call_empty_loop instr/iter:", ce)
    bench_loop("call_empty_loop", call_empty_loop, 1_000_000, per_iter_instr=ce)


if __name__ == "__main__":
    main()
