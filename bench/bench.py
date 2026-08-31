"""Cross-interpreter benchmark suite: RustPython vs CPython.
Usage: python bench.py / rustpython bench.py
Each benchmark is time-boxed; prints ops/sec so higher = better.
"""
import time
import sys

IMPL = sys.implementation.name

def bench(name, fn, seconds=1.0):
    # calibrate
    n = 1
    while True:
        t0 = time.perf_counter()
        fn(n)
        dt = time.perf_counter() - t0
        if dt > 0.05 or n >= 10_000_000:
            break
        n *= 10
    # measure
    count = 0
    total = 0.0
    while total < seconds:
        t0 = time.perf_counter()
        fn(n)
        total += time.perf_counter() - t0
        count += n
    ops = count / total
    print(f"{name:28s} {ops:14,.0f} ops/s")
    return ops

# 1. pure function call overhead
def f(a, b):
    return a + b
def bench_call(n):
    for _ in range(n):
        f(1, 2)

# 2. method call (attribute lookup + call)
class C:
    def m(self, a):
        return a + 1
obj = C()
def bench_method(n):
    for _ in range(n):
        obj.m(3)

# 3. integer arithmetic loop
def bench_arith(n):
    s = 0
    for i in range(n):
        s += i * 2 - 1
    return s

# 4. list build + index
def bench_list(n):
    lst = list(range(1000))
    s = 0
    for _ in range(n):
        s += lst[7] + lst[993]

# 5. dict get/set
d = {("k%d" % i): i for i in range(100)}
def bench_dict(n):
    for _ in range(n):
        d["k50"] = d["k60"] + 1

# 6. string concat / split small
def bench_str(n):
    s = "abcdefgh"
    for _ in range(n):
        s2 = s.upper() + "ij"
        s2.split("d")

# 7. sorting (random lists, size 500)
import random
random.seed(42)
data = [random.random() for _ in range(500)]
def bench_sort(n):
    for _ in range(n):
        x = data.copy()
        x.sort()

# 8. exceptions (try/except raise cost)
def throw(i):
    raise ValueError(i)
def bench_exc(n):
    caught = 0
    for i in range(n):
        try:
            throw(i)
        except ValueError:
            caught += 1

# 9. class instantiation
def bench_class(n):
    for _ in range(n):
        C()

# 10. generator iteration
def gen():
    yield 1
    yield 2
def bench_gen(n):
    for _ in range(n):
        g = gen()
        next(g)
        next(g)

# 11. list comprehension
def bench_compr(n):
    for _ in range(n):
        [i * i for i in range(100)]

# 12. string formatting
def bench_fmt(n):
    for _ in range(n):
        "%s=%d" % ("x", 42)

print(f"=== benchmark on {IMPL} (python {sys.version.split()[0]}) ===")
results = {}
results["call"]          = bench("function call",        bench_call)
results["method"]        = bench("method call",          bench_method)
results["arith"]         = bench("int arithmetic loop",  bench_arith, 2.0)
results["list_index"]    = bench("list index",           bench_list)
results["dict"]          = bench("dict get/set",         bench_dict)
results["str"]           = bench("string ops",           bench_str)
results["sort500"]       = bench("sort 500 floats",      bench_sort, 2.0)
results["exception"]     = bench("raise/catch exc",      bench_exc, 2.0)
results["class_new"]     = bench("class instantiate",    bench_class)
results["generator"]     = bench("generator next",       bench_gen)
results["compr"]         = bench("list comprehension",   bench_compr)
results["format"]        = bench("string format",        bench_fmt)
print("=== done ===")
