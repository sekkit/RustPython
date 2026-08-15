import time
def a4(n: int) -> int:
    s = 0
    i = 0
    while i < n:
        s = s + i
        i = i + 1
    return s
a4.__jit__()
N = 100_000_000
t0=time.perf_counter(); r = a4(N); t1=time.perf_counter()
print(f"jit   : {t1-t0:.3f}s ({r})")
