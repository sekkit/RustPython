# Bytecode instruction-level cost: RustPython vs CPython
# Each loop body isolates one bytecode-heavy operation cost per iteration.
import time

N = 1_000_000

def bench(name, fn, iters=N):
    t0 = time.perf_counter()
    r = fn(iters)
    t1 = time.perf_counter()
    ns = (t1 - t0) / iters * 1e9
    print(f'{name}: {ns:.1f} ns/iter  ({r})')
    return ns

# LOAD_FAST + STORE_FAST loop counter (2 instr per iter)
def loop_counter(n):
    i = 0
    while i < n:
        i += 1
    return i
bench('while_counter', loop_counter, 3_000_000)

# BINARY_OP int add
def int_add_loop(n):
    s = 0
    for i in range(n):
        s += i
    return s
bench('for_int_add', int_add_loop)

# COMPARE_OP int < int
def cmp_loop(n):
    s = 0
    for i in range(n):
        if i < n: s += 1
    return s
bench('compare_lt', cmp_loop, 500_000)

# LOAD_ATTR (method lookup)
class C:
    def __init__(self): self.x = 1
c = C()
def attr_loop(n):
    s = 0
    for _ in range(n):
        s += c.x
    return s
bench('load_attr', attr_loop)

# CALL_METHOD (bound method)
class D:
    def m(self): return 1
d = D()
def method_loop(n):
    s = 0
    for _ in range(n):
        s += d.m()
    return s
bench('call_method', method_loop, 500_000)

# BUILD_LIST + list append via BINARY_SUBSCR/STORE_SUBSCR
def list_subscr_loop(n):
    lst = [0, 1, 2, 3]
    s = 0
    for i in range(n):
        s += lst[i & 3]
    return s
bench('list_subscr', list_subscr_loop)

# exception-free try (SETUP + POP block)
def try_loop(n):
    s = 0
    for i in range(n):
        try:
            s += i
        except Exception:
            s += 1
    return s
bench('try_pass', try_loop, 500_000)

# FOR_ITER over list
def for_iter(n):
    lst = list(range(10))
    s = 0
    for _ in range(n):
        for x in lst:
            s += x
    return s
bench('for_iter10', for_iter, 200_000)