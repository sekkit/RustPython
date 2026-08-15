import time
def a4(n):
    s = 0; i = 0
    while i < n:
        s = s + i; i = i + 1
    return s
t0=time.perf_counter(); r = a4(3_000_000_000); t1=time.perf_counter()
print(f"interp n=3e9: {r} in {t1-t0:.2f}s")
