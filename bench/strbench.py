import time
# 细粒度对比字符串操作
def bench(fn, n, label):
    t0 = time.perf_counter(); fn(n); dt = time.perf_counter() - t0
    print(f"{label:24s} {dt:.3f}s")
s = "abcdefghij" * 10
def b_upper(n):
    for _ in range(n): s.upper()
def b_lower(n):
    for _ in range(n): s.lower()
def b_split(n):
    for _ in range(n): s.split("e")
def b_join(n):
    for _ in range(n): "-".join(["a","b","c"])
def b_format(n):
    for _ in range(n): "{}:{}".format(1,2)
def b_find(n):
    for _ in range(n): s.find("fg")
def b_startswith(n):
    for _ in range(n): s.startswith("abc")
def b_slice(n):
    for _ in range(n): s[3:7]
def b_replace(n):
    for _ in range(n): s.replace("ab","XY")
def b_len(n):
    for _ in range(n): len(s)
import sys
N = 200_000
print("=== on", sys.implementation.name, "===")
bench(b_upper, N, "str.upper x200k")
bench(b_lower, N, "str.lower x200k")
bench(b_split, N, "str.split x200k")
bench(b_join, N, "str.join x200k")
bench(b_format, N, "str.format x200k")
bench(b_find, N, "str.find x200k")
bench(b_startswith, N, "str.startswith x200k")
bench(b_slice, N, "str slice x200k")
bench(b_replace, N, "str.replace x200k")
bench(b_len, N, "len(str) x200k")
