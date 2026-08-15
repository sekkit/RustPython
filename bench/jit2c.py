import time
def fib(n: int) -> int:
    if n < 2: return n
    return fib(n-1) + fib(n-2)
t0 = time.perf_counter(); fib(28); t1 = time.perf_counter()
print(f"cpython fib(28): {t1-t0:.3f}s")
def loop(n: int) -> int:
    s = 0
    for i in range(n):
        s = s + i * 2
    return s
t0 = time.perf_counter(); loop(3_000_000); t1 = time.perf_counter()
print(f"cpython loop: {t1-t0:.3f}s")
