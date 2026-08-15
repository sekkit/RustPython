import sqlite3
conn = sqlite3.connect(":memory:", uri=True)
try:
    r = conn.execute("select sqlite_compileoption_used('ENABLE_MATH_FUNCTIONS')").fetchone()
    print("uri=True compileoption:", r)
except Exception as e:
    print("uri=True ERR:", type(e).__name__, e)
conn2 = sqlite3.connect("file::memory:?cache=shared", uri=True)
try:
    r = conn2.execute("select sqlite_compileoption_used('ENABLE_MATH_FUNCTIONS')").fetchone()
    print("file-uri compileoption:", r)
except Exception as e:
    print("file-uri ERR:", type(e).__name__, e)
