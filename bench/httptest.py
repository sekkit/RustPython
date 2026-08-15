import http.client
try:
    c = http.client.HTTPConnection("localhost\r\nX-injected: header\r\n", 8080)
    print("NO RAISE - host =", repr(c.host))
except http.client.InvalidURL as e:
    print("InvalidURL:", e)
except Exception as e:
    print("OTHER:", type(e).__name__, e)
