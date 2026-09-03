# 补充维度：异常与字符串操作成本（确认无遗漏可节流项）
import time

N = 500_000

def bench(name, fn, it=N):
    t0 = time.perf_counter()
    r = fn(it)
    t1 = time.perf_counter()
    print(f'{name}: {(t1-t0)/it*1e9:.1f} ns/op  ({r})')

# 异常抛出+捕获完整往返
def raise_catch(n):
    s = 0
    for i in range(n):
        try:
            raise ValueError('x')
        except ValueError:
            s += 1
    return s
bench('raise_catch', raise_catch)

# 无异常时的 try 块（纯 SETUP 开销）
def try_noexc(n):
    s = 0
    for i in range(n):
        try:
            s += i
        except Exception:
            s += 1
    return s
bench('try_noexc', try_noexc)

# 字符串拼接 +
def str_concat(n):
    s = ''
    for i in range(100):
        s = 'a' + s
    return len(s)
bench('str_concat100', str_concat, 20000)

# 字符串 join
def str_join(n):
    parts = ['a'] * 10
    for _ in range(n):
        s = ''.join(parts)
    return len(s)
bench('str_join10', str_join)

# 字符串格式化 %s
def str_format(n):
    s = ''
    for i in range(n):
        s = '%d-%s' % (i, 'x')
    return len(s)
bench('str_percent_format', str_format, 200000)