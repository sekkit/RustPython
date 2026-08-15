import time
def bench(fn, n, label):
    t0 = time.perf_counter(); fn(n); dt = time.perf_counter() - t0
    print(f"{label:34s} {dt*1000:8.1f}ms  ({n/dt:,.0f} it/s)")
N = 3_000_000

def b_empty(n):
    for i in range(n): pass
def b_load(n):
    s = 0
    for i in range(n): s = i
def b_arith(n):
    s = 0
    for i in range(n): s += i
def f(a, b): return a + b
def b_call(n):
    s = 0
    for i in range(n): s += f(1, 2)
def b_call_noarg(n):
    s = 0
    for i in range(n): s = f(1, 2)
def b_method(n):
    s = 0
    for i in range(n): s = s.__add__(1)

bench(b_empty, N, "for loop only")
bench(b_load, N, "+ load/store local")
bench(b_arith, N, "+ int add")
bench(b_call, N, "+ call f(1,2) & use")
bench(b_call_noarg, N, "+ call f(1,2) discard")
bench(b_method, N, "+ method call __add__")
