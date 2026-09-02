import time

N = 1_000_000

# 单形态：同一函数反复调用（specialization 缓存应命中）
def f0(x):
    return x + 1
t0 = time.perf_counter()
s = 0
for i in range(N):
    s += f0(i)
t1 = time.perf_counter()
mono = (t1 - t0) / N * 1e9
print(f'monomorphic_call: {mono:.1f} ns  s={s}')

# 多形态：交替调用两个不同函数（缓存轮流失效/重新适配）
def g1(x):
    return x * 2
def g2(x):
    return x * 3
fs = [g1, g2]
t0 = time.perf_counter()
s = 0
for i in range(N):
    s += fs[i & 1](i)
t1 = time.perf_counter()
poly = (t1 - t0) / N * 1e9
print(f'polymorphic_call: {poly:.1f} ns  s={s}')

# 超多形态：8 个函数轮换（缓存完全失效）
import types
funcs = [types.FunctionType(f0.__code__, {}, f'f{i}') for i in range(4)] + [g1, g2]
def h1(x): return x + 5
def h2(x): return x + 6
funcs = funcs + [h1, h2]
t0 = time.perf_counter()
s = 0
for i in range(N):
    s += funcs[i & 7](i)
t1 = time.perf_counter()
mega = (t1 - t0) / N * 1e9
print(f'megamorphic_call: {mega:.1f} ns  s={s}')

print(f'poly/mono ratio: {poly/mono:.2f}x  mega/mono: {mega/mono:.2f}x')