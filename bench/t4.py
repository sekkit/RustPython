import math
# exhaustive-ish probe of the failing values in testLdexp_denormal
bad = []
for e in range(-1200, -1050):
    for m in (1.0, 1.5, 2.0, 3.0, 7.0):
        try:
            v = math.ldexp(m, e)
        except OverflowError:
            continue
        # reference computed by repeated halving (independent path)
        ref = m
        ok = True
        try:
            for _ in range(-e):
                ref /= 2.0
        except OverflowError:
            continue
        if v != ref and not (v != v):
            bad.append((m, e, v, ref))
print("mismatches:", len(bad))
for row in bad[:6]:
    print(row)
