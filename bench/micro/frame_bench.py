import time

N = 400_000

# 深度递归：隔离"帧创建+栈管理"成本（无参数无返回计算）
def deep(n):
    if n <= 0:
        return
    deep(n-1)
t0 = time.perf_counter()
deep(500)  # 低于递归深度限制
t1 = time.perf_counter()
print(f'deep_recursion_500: {(t1-t0)*1e6/500:.1f} us per level  (total {(t1-t0)*1000:.1f} ms)')

# 尾递归风格：两层嵌套调用 vs 平铺调用
def lvl1():
    pass
def lvl2():
    lvl1()
t0 = time.perf_counter()
for _ in range(N):
    lvl2()
t1 = time.perf_counter()
print(f'two_level_call: {(t1-t0)/N*1e9:.1f} ns/op')

# 迭代 vs 递归帧复用（同工作量）
def loop_sum(n):
    s = 0
    for i in range(n):
        s += i
    return s
t0 = time.perf_counter()
loop_sum(20000)
t1 = time.perf_counter()
print(f'loop20000: {(t1-t0)*1e6:.1f} us (单帧, 解释器循环)')