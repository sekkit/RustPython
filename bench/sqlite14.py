import sqlite3
# 逐步叠加参数
for kwargs in [
    {},
    {"detect_types": 3},
    {"check_same_thread": False},
    {"uri": True},
    {"detect_types": 3, "check_same_thread": False},
    {"detect_types": 3, "check_same_thread": False, "uri": True},
]:
    conn = sqlite3.connect(":memory:", **kwargs)
    conn.create_function("T", 1, lambda x: x * 2)
    try:
        r = conn.execute("select T(3)").fetchone()
        print(f"{kwargs}: ok {r}")
    except Exception as e:
        print(f"{kwargs}: ERR {type(e).__name__}: {e}")
