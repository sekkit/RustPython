import http.client
# 模拟 _get_hostport 处理
c = http.client.HTTPConnection("localhost\x00")
print("char00 host =", repr(c.host))
try:
    c2 = http.client.HTTPConnection("localhost\x01")
    print("char01 host =", repr(c2.host), "NO RAISE")
except Exception as e:
    print("char01:", type(e).__name__, str(e)[:60])
