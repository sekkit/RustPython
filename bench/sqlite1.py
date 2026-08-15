import sqlite3
conn = sqlite3.connect(":memory:")
conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
conn.execute("INSERT INTO t (name) VALUES (?)", ("rustpython",))
conn.commit()
rows = conn.execute("SELECT * FROM t").fetchall()
print("sqlite rows:", rows)
# 事务回滚
try:
    conn.execute("INSERT INTO t (name) VALUES (?)", (None,))
    conn.execute("INSERT INTO t (name) VALUES ('x')")
    conn.commit()
except Exception as e:
    print("constraint error ok:", type(e).__name__)
print("sqlite OK")
