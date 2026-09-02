import time, sys

N = 1_500_000

# A: plain call to module-level function (dispatch + frame + name lookup once)
def f0():
    pass
t0 = time.perf_counter()
for _ in range(N):
    f0()
t1 = time.perf_counter()
print(f'A_plain_call: {(t1-t0)/N*1e9:.1f} ns')

# B: call via local alias (fewer name lookups in loop; same as A effectively)
g = f0
t0 = time.perf_counter()
for _ in range(N):
    g()
t1 = time.perf_counter()
print(f'B_local_alias: {(t1-t0)/N*1e9:.1f} ns')

# C: call through a closure cell (cell var access in call)
def make():
    def inner():
        pass
    return inner
h = make()
t0 = time.perf_counter()
for _ in range(N):
    h()
t1 = time.perf_counter()
print(f'C_closure_call: {(t1-t0)/N*1e9:.1f} ns')

# D: call with 3 args (args tuple building cost)
def f3(a, b, c):
    return a+b+c
t0 = time.perf_counter()
for i in range(N):
    f3(i, i+1, i+2)
t1 = time.perf_counter()
print(f'D_3args: {(t1-t0)/N*1e9:.1f} ns')

# E: call with *args (tuple unpacking + build)
def fv(*args):
    return args
t0 = time.perf_counter()
for i in range(N):
    fv(i, i+1, i+2)
t1 = time.perf_counter()
print(f'E_varargs: {(t1-t0)/N*1e9:.1f} ns')

# F: global name lookup each iteration (name resolution cost)
t0 = time.perf_counter()
for _ in range(N):
    f0  # just LOAD_GLOBAL
t1 = time.perf_counter()
print(f'F_global_load: {(t1-t0)/N*1e9:.1f} ns')

# G: recursion to depth 1 (two frames)
def r(n):
    if n <= 0:
        return 0
    return r(n-1)
t0 = time.perf_counter()
for _ in range(N):
    r(1)
t1 = time.perf_counter()
print(f'G_rec1: {(t1-t0)/N*1e9:.1f} ns')