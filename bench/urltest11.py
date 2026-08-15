import http.client, ssl
host = "localhost\r\nX-injected: header\r\n:8080"
ctx = ssl.create_default_context()
try:
    h = http.client.HTTPSConnection(host, timeout=5, context=ctx)
    print("NO RAISE host =", repr(h.host))
except Exception as e:
    print("raised:", type(e).__name__, str(e)[:70])
