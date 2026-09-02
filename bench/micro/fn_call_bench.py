import time

def empty():
    pass

def add1(x):
    return x + 1

def fib(n):
    if n < 2:
        return n
    return fib(n-1) + fib(n-2)

N = 2_000_000

# 1. plain function call
t0 = time.perf_counter()
for _ in range(N):
    empty()
t1 = time.perf_counter()
print(f'empty_call: {(t1-t0)/N*1e9:.1f} ns/call')

# 2. call with arg
t0 = time.perf_counter()
s = 0
for i in range(N):
    s += add1(i)
t1 = time.perf_counter()
print(f'add1_call: {(t1-t0)/N*1e9:.1f} ns/call  s={s}')

# 3. method call
class C:
    def m(self, x):
        return x + 1
c = C()
t0 = time.perf_counter()
s = 0
for i in range(N):
    s += c.m(i)
t1 = time.perf_counter()
print(f'method_call: {(t1-t0)/N*1e9:.1f} ns/call  s={s}')

# 4. recursion (fib(24))
t0 = time.perf_counter()
r = fib(24)
t1 = time.perf_counter()
print(f'fib(24): {(t1-t0)*1000:.1f} ms  r={r}')

# 5. int add
t0 = time.perf_counter()
s = 0
for i in range(N):
    s += i
t1 = time.perf_counter()
print(f'int_add: {(t1-t0)/N*1e9:.1f} ns/op  s={s}')

# 6. int mul
t0 = time.perf_counter()
s = 1
for i in range(1, N):
    s ^= i * 3
t1 = time.perf_counter()
print(f'int_mul: {(t1-t0)/N*1e9:.1f} ns/op  s={s}')

# 7. list append
t0 = time.perf_counter()
lst = []
for i in range(1_000_000):
    lst.append(i)
t1 = time.perf_counter()
print(f'list_append: {(t1-t0)/1e6*1e9:.1f} ns/op  len={len(lst)}')

# 8. dict get/set
t0 = time.perf_counter()
d = {}
for i in range(1_000_000):
    d[i] = i
    _ = d[i]
t1 = time.perf_counter()
print(f'dict_setget: {(t1-t0)/1e6*1e9:.1f} ns/op  len={len(d)}')