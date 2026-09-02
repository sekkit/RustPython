# Decompose closure creation 7.1x gap
import time

N = 500_000

def bench(name, fn):
    t0 = time.perf_counter()
    r = fn(N)
    t1 = time.perf_counter()
    print(f'{name}: {(t1-t0)/N*1e9:.1f} ns/iter  ({r})')

# baseline: function with no closure (def inside loop)
def no_closure(n):
    s = 0
    for _ in range(n):
        def f():
            return 1
        s += f()
    return s
bench('def_no_capture', no_closure)

# closure capturing 1 loop var (1 cell)
def one_capture(n):
    s = 0
    for i in range(n):
        def f(x=i):
            return x
        s += f()
    return s
bench('closure_1_capture', one_capture)

# closure capturing 2 vars (2 cells)
def two_capture(n):
    s = 0
    j = 1
    for i in range(n):
        def f(x=i, y=j):
            return x + y
        s += f()
    return s
bench('closure_2_capture', two_capture)

# cell creation only (MAKE_CELL, no function)
def make_cell_only(n):
    s = 0
    for i in range(n):
        # emulate MAKE_CELL via a nested function that returns a cell
        c = [i]
        s += c[0]
    return s
bench('list_as_cell', make_cell_only)

# function object creation only (module-level def, cached)
def cached_func(n):
    s = 0
    for _ in range(n):
        f = _cached  # no new function
        s += f()
    return s
def _cached():
    return 1
bench('reuse_cached_func', cached_func)