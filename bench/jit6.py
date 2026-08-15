import time
def a4(n: int) -> int:
    s = 0
    i = 0
    while i < n:
        s = s + i
        i = i + 1
    return s
N = 30_000_000
t0=time.perf_counter(); a4(N); t1=time.perf_counter()
interp = t1-t0
a4.__jit__()
t0=time.perf_counter(); a4(N); t1=time.perf_counter()
jit = t1-t0
print(f"interp while-loop {N}: {interp:.3f}s")
print(f"jit    while-loop {N}: {jit:.3f}s   speedup: {interp/jit:.2f}x")
