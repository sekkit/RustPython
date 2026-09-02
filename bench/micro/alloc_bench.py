# Instance/object allocation cost: RustPython vs CPython 3.12
import time

N = 1_000_000

def bench(name, fn, iters=N):
    t0 = time.perf_counter()
    r = fn(iters)
    t1 = time.perf_counter()
    ns = (t1 - t0) / iters * 1e9
    print(f'{name}: {ns:.1f} ns/alloc  ({r})')
    return ns

# plain class instantiation (no __init__)
class Empty:
    pass
def alloc_empty(n):
    lst = []
    for _ in range(n):
        lst.append(Empty())
    return len(lst)
bench('class_empty', alloc_empty, 500_000)

# class with __init__ setting 2 attrs
class Pair:
    __slots__ = ('a', 'b')
    def __init__(self, a, b):
        self.a = a
        self.b = b
def alloc_pair(n):
    lst = []
    for i in range(n):
        lst.append(Pair(i, i+1))
    return len(lst)
bench('class_slots_pair', alloc_pair, 500_000)

# tuple alloc (2 items)
def alloc_tuple(n):
    s = 0
    for i in range(n):
        t = (i, i+1)
        s += t[0]
    return s
bench('tuple2', alloc_tuple)

# list alloc + 1 append
def alloc_list1(n):
    s = 0
    for i in range(n):
        lst = [i]
        s += lst[0]
    return s
bench('list1', alloc_list1)

# dict alloc + 1 set
def alloc_dict1(n):
    s = 0
    for i in range(n):
        d = {i: i}
        s += d[i]
    return s
bench('dict1', alloc_dict1, 500_000)

# closure creation
def alloc_closure(n):
    s = 0
    for i in range(n):
        def f(x=i):
            return x
        s += f()
    return s
bench('closure', alloc_closure, 500_000)

# generator object creation (no iteration)
def gen():
    yield 1
def alloc_gen(n):
    s = 0
    for _ in range(n):
        g = gen()
        s += 1
    return s
bench('generator_obj', alloc_gen, 500_000)