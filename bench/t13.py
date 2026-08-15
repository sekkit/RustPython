import math
from fractions import Fraction
cases = [(1,0),(float('inf'),1),(float('nan'),1),(1,float('nan'))]
for x,y in cases:
    try:
        math.remainder(x,y)
        print(x,y,"-> ok(no raise)")
    except ValueError as e:
        print(x,y,"-> ValueError:", str(e))
