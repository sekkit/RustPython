import time
def fib(n):
    if n < 2: return n
    return fib(n-1) + fib(n-2)

# before jit
t0 = time.perf_counter(); fib(28); t1 = time.perf_counter()
print(f"interpreted fib(28): {t1-t0:.3f}s")
try:
    fib.__jit__()
    t0 = time.perf_counter(); fib(28); t1 = time.perf_counter()
    print(f"jitted fib(28):     {t1-t0:.3f}s")
except Exception as e:
    print("jit failed:", type(e).__name__, str(e)[:100])
