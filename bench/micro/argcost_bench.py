# Vec-alloc vs inline: cost of per-call args vector allocation
import time

N = 2_000_000

def f0():
    pass
def f1(a):
    return a
def f3(a, b, c):
    return a + b + c

t0 = time.perf_counter()
for _ in range(N):
    f0()
t1 = time.perf_counter()
c0 = (t1-t0)/N*1e9

t0 = time.perf_counter()
s = 0
for i in range(N):
    s += f1(i)
t1 = time.perf_counter()
c1 = (t1-t0)/N*1e9

t0 = time.perf_counter()
s = 0
for i in range(N):
    s += f3(i, i+1, i+2)
t1 = time.perf_counter()
c3 = (t1-t0)/N*1e9

print(f'empty_call: {c0:.1f} ns')
print(f'1arg_call: {c1:.1f} ns  (delta from empty: {c1-c0:.1f})')
print(f'3arg_call: {c3:.1f} ns  (delta from 1arg: {c3-c1:.1f} = per-extra-arg)')