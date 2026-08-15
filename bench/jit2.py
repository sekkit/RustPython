import time
def fib(n: int) -> int:
    if n < 2: return n
    return fib(n-1) + fib(n-2)

t0 = time.perf_counter(); r1 = fib(28); t1 = time.perf_counter()
print(f"interpreted fib(28) = {r1}: {t1-t0:.3f}s")
fib.__jit__()
t0 = time.perf_counter(); r2 = fib(28); t1 = time.perf_counter()
print(f"jitted      fib(28) = {r2}: {t1-t0:.3f}s")
# 带类型注解的简单数值循环
def loop(n: int) -> int:
    s = 0
    for i in range(n):
        s = s + i * 2
    return s
t0 = time.perf_counter(); r3 = loop(3_000_000); t1 = time.perf_counter()
print(f"interpreted loop = {r3}: {t1-t0:.3f}s")
loop.__jit__()
t0 = time.perf_counter(); r4 = loop(3_000_000); t1 = time.perf_counter()
print(f"jitted      loop = {r4}: {t1-t0:.3f}s")
