import time
def fib(n: int) -> int:
    if n < 2: return n
    return fib(n-1) + fib(n-2)
t0 = time.perf_counter(); r1 = fib(26); t1 = time.perf_counter()
print(f"interp fib(26) = {r1}: {t1-t0:.4f}s")
fib.__jit__()
t0 = time.perf_counter(); r2 = fib(26); t1 = time.perf_counter()
print(f"jit    fib(26) = {r2}: {t1-t0:.4f}s")
def add2(a: int, b: int) -> int:
    return a + b
t0 = time.perf_counter()
for _ in range(1_000_000): add2(1, 2)
t1 = time.perf_counter()
print(f"interp add2 x1M: {t1-t0:.4f}s")
add2.__jit__()
t0 = time.perf_counter()
for _ in range(1_000_000): add2(1, 2)
t1 = time.perf_counter()
print(f"jit    add2 x1M: {t1-t0:.4f}s")
