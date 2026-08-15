funcs = {
  "add2":      lambda: None,
}
def try_jit(name, fn):
    try:
        fn.__jit__()
        return "JIT-OK"
    except Exception as e:
        return "no: " + str(e)[:60]
def a1(a: int) -> int: return a + 1
def a2(a: int, b: int) -> int: return a * b + a - b
def a3(a: float) -> float: return a * 2.0
def a4(n: int) -> int:
    s = 0
    i = 0
    while i < n:
        s = s + i
        i = i + 1
    return s
def a5(n: int) -> int:
    if n > 3:
        return 1
    return 2
print("a1 simple add      :", try_jit("a1", a1))
print("a2 multi-arith     :", try_jit("a2", a2))
print("a3 float           :", try_jit("a3", a3))
print("a4 while loop      :", try_jit("a4", a4))
print("a5 if branch       :", try_jit("a5", a5))
