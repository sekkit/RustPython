import time, sys, json

results = {}

def bench(name, fn, iterations=1000, warmup=50):
    for _ in range(warmup):
        fn()
    best = float("inf")
    for _ in range(3):
        start = time.perf_counter()
        for _ in range(iterations):
            fn()
        elapsed = time.perf_counter() - start
        best = min(best, elapsed)
    results[name] = round(best * 1e6 / iterations, 2)

# === INTEGER ===
bench("int_add", lambda: 12345678 + 87654321, 100000)
bench("bigint_add", lambda: 10**100 + 10**99, 50000)

# === FLOAT ===
bench("float_add", lambda: 1.5 + 2.7, 100000)
bench("float_div", lambda: 10.0 / 3.0, 100000)

# === STRING SHORT ===
bench("str_concat_short", lambda: "hello" + "world", 100000)
bench("str_eq_short", lambda: "hello" == "world", 100000)

# === STRING LONG (22KB) ===
S = "The quick brown fox jumps over the lazy dog. " * 500
bench("str_upper_long", lambda: S.upper(), 200)
bench("str_find_long", lambda: S.find("lazy dog"), 500)
bench("str_count_long", lambda: S.count("the"), 200)
bench("str_split_long", lambda: S.split(), 50)
bench("str_replace_long", lambda: S.replace("fox", "cat"), 200)
bench("str_startswith_long", lambda: S.startswith("The"), 50000)
bench("str_strip_long", lambda: (" " + S + " ").strip(), 100)

# === LIST ===
L = list(range(10000))
bench("list_getitem", lambda: L[5000], 50000)
bench("list_append", lambda: [].append(1), 50000)
bench("list_contains", lambda: 9999 in L, 500)
bench("list_slice", lambda: L[100:200], 5000)
bench("list_sort_1k", lambda: sorted(L[:1000]), 20)
bench("list_sum", lambda: sum(L), 50)

# === DICT ===
D = {f"key_{i}": i for i in range(10000)}
bench("dict_getitem", lambda: D["key_5000"], 50000)
bench("dict_contains", lambda: "key_5000" in D, 50000)
bench("dict_keys_list", lambda: list(D.keys()), 100)

# === TUPLE ===
T = tuple(range(10000))
bench("tuple_getitem", lambda: T[5000], 50000)
bench("tuple_in", lambda: 5000 in T, 200)

# === CALLS / CLASSES / ITERATION ===
def empty_fn(): pass
class Simple:
    attr = 42
    def method(self): return self.attr
obj = Simple()
bench("fn_call_empty", lambda: empty_fn(), 100000)
bench("method_call", lambda: obj.method(), 100000)
bench("isinstance_check", lambda: isinstance(obj, Simple), 50000)
bench("for_loop_range_1k", lambda: sum(range(1000)), 200)
bench("list_compr_1k", lambda: [x*2 for x in range(1000)], 50)

# === EXCEPTIONS ===
def try_except():
    try:
        raise ValueError("test")
    except ValueError:
        pass
bench("raise_catch", lambda: try_except(), 10000)

print(json.dumps(results))