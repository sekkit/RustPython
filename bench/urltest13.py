import urllib.request, http.client
orig = http.client.HTTPSConnection.__init__
def spy(self, host, *a, **k):
    print("HTTPSConnection.__init__ host =", repr(host))
    return orig(self, host, *a, **k)
http.client.HTTPSConnection.__init__ = spy
try:
    urllib.request.urlopen("https://localhost\r\nX-injected: header\r\n:8080/test/?test=a", timeout=3)
except Exception as e:
    print("raised:", type(e).__name__, str(e)[:60])
