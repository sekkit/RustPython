import struct
v = 6993274598585239 * 2.0**-1126   # 纯 Python 计算,双精度
import math
l = math.ldexp(6993274598585239, -1126)
print("py mul :", repr(v), hex(struct.unpack("<Q", struct.pack("<d", v))[0]))
print("ldexp  :", repr(l), hex(struct.unpack("<Q", struct.pack("<d", l))[0]))
