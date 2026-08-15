import http.client
# 模拟测试的 fakehttp 替换
orig = http.client.HTTPConnection
class FakeHTTPConnection(http.client.HTTPConnection):
    buf = None
    def connect(self):
        self.sock = None
http.client.HTTPConnection = FakeHTTPConnection
try:
    c = http.client.HTTPSConnection("localhost\x00")
    print("NO RAISE, host =", repr(c.host))
except Exception as e:
    print("raised:", type(e).__name__, str(e)[:60])
finally:
    http.client.HTTPConnection = orig
