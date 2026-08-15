import time
def a4(n: int) -> int:
    s = 0
    i = 0
    while i < n:
        s = s + i
        i = i + 1
    return s
a4.__jit__()
# 单次大 n:摊薄 per-call 开销
t0=time.perf_counter(); a4(3_000_000_000); t1=time.perf_counter()
print(f"jit single call n=3e9: {t1-t0:.3f}s")
t0=time.perf_counter(); a4(0); t1=time.perf_counter()
print(f"jit call overhead (n=0): {(t1-t0)*1e6:.2f} us")
def a0(n: int) -> int:
    return n
t0=time.perf_counter()
for _ in range(100000): a0(1)
t1=time.perf_counter()
print(f"empty fn per-call: {(t1-t0)/100000*1e6:.2f} us")
a0.__jit__()
t0=time.perf_counter()
for _ in range(100000): a0(1)
t1=time.perf_counter()
print(f"jit empty fn per-call: {(t1-t0)/100000*1e6:.2f} us")
