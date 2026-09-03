# 补充维度：生成器与上下文管理器成本（确认无遗漏可节流项）
import time

N = 500_000

def bench(name, fn, it=N):
    t0 = time.perf_counter()
    r = fn(it)
    t1 = time.perf_counter()
    print(f'{name}: {(t1-t0)/it*1e9:.1f} ns/op  ({r})')

# 生成器创建 + 迭代完整往返
def gen_roundtrip(n):
    s = 0
    for i in range(n):
        g = (x for x in range(3))
        s += sum(g)
    return s
bench('gen_roundtrip', gen_roundtrip, 100000)

# 生成器创建但不迭代（仅对象创建）
def gen_create(n):
    s = 0
    for i in range(n):
        g = (x for x in range(3))
        s += 1
    return s
bench('gen_create', gen_create, 200000)

# 生成器 yield 单步
def gen_step(n):
    def inner():
        for i in range(3):
            yield i
    s = 0
    for i in range(n):
        g = inner()
        s += next(g)
    return s
bench('gen_yield_step', gen_step, 100000)

# 上下文管理器进入/退出（无异常）
from contextlib import contextmanager
@contextmanager
def ctx():
    yield
def ctx_roundtrip(n):
    s = 0
    for i in range(n):
        with ctx():
            s += 1
    return s
bench('ctx_roundtrip', ctx_roundtrip, 100000)

# with 语句内建类型（文件类协议，用 self 简化）
class C:
    def __enter__(self): return self
    def __exit__(self, *a): return False
c = C()
def with_builtin(n):
    s = 0
    for i in range(n):
        with c:
            s += 1
    return s
bench('with_builtin', with_builtin, 200000)