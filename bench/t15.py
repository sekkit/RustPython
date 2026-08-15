# 直接看 testRemainder 全部子断言哪些 raise:用 assertRaises 的消息
import math, unittest, inspect
import test.test_math as tm
src = inspect.getsource(tm.MathTests.testRemainder)
# 手动重放 raise 用例
for x, y in [(float("inf"), 1), (1, float("inf")), (float("inf"), float("inf"))]:
    try:
        math.remainder(x, y); print(x, y, "-> no raise")
    except ValueError as e:
        print(x, y, "-> ValueError:", repr(str(e)))
for x, y in [(float("nan"), 1), (1, float("nan"))]:
    r = math.remainder(x, y)
    print(x, y, "->", r)
