import math
# 找出 testRemainder 里 raise 的具体子用例:遍历测试体逻辑
strs = ["1e5", "1e15", "1e300"]
try:
    math.remainder(float("1e5"), float("1e15"))
except ValueError as e:
    print("case1:", str(e))
# 检查错误消息格式:CPython 对 remainder(1, 0) 也是 math domain error
# 那个标记说 "Error message too long" —— 可能在 fmod 上
import itertools, random
random.seed(1)
mismatch = 0
for _ in range(2000):
    x = random.uniform(-1e10, 1e10); y = random.uniform(-1e10, 1e10)
    if y == 0: continue
    r = math.remainder(x, y)
    # spec check
    if not (abs(r) <= abs(y)/2 + abs(y)*1e-15):
        mismatch += 1
        print("spec violation", x, y, r)
        break
print("random spec check:", "OK" if mismatch == 0 else "FAIL")
