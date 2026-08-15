import http.client
# 模拟 do_open 的调用:host 含 :8080,port=None
try:
    c = http.client.HTTPSConnection("localhost\r\nX-injected: header\r\n:8080")
    print("NO RAISE! host =", repr(c.host), "port =", c.port)
except Exception as e:
    print("raised:", type(e).__name__, str(e)[:70])
