import math
try:
    math.remainder(1, 0)
except ValueError as e:
    print("msg:", repr(str(e)))
except Exception as e:
    print("other:", type(e).__name__, repr(str(e)))
