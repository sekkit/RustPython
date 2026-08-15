import struct
def bits(x): return struct.unpack("<Q", struct.pack("<d", x))[0]
# 6993274598585239 = 0x18E_...  exact value = 0x18E... * 2^-1126
# 期望:精确值 ≈ 1.5587 * 2^-1073,最近偶数舍入 → 2^-1073 = 1e-323 (0x2)
import math
l = math.ldexp(6993274598585239, -1126)
print("ldexp bits:", hex(bits(l)), "expect 0x2 (1e-323)")
# frexp 分解验证
m, e = math.frexp(6993274598585239)
print("frexp:", m, e)  # 0.5<=m<1
# 精确值 = m * 2^(e-1126);m*2^0.?
