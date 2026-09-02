# Probe: show current (possibly specialized) opcodes at the call sites
# after warmup, on the interpreter that's running this script.
import dis


def make(k):
    g = {}
    for j in range(k):
        exec(f"def f{j}(x):\n    return x + {j}", g)
    return [g[f"f{j}"] for j in range(k)]


FS1 = make(1)
FSK = make(16)


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


# warmup so specialization kicks in
bench_mono(200_000)
bench_mega(200_000)

print("== bench_mono (adaptive) ==")
dis.dis(bench_mono.__code__, adaptive=True)
print("== bench_mega (adaptive) ==")
dis.dis(bench_mega.__code__, adaptive=True)