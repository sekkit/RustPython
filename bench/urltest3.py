import urllib.request
req = urllib.request.Request("https://localhost\x00/test/")
print("tunnel_host:", repr(req._tunnel_host))
print("host:", repr(req.host))
# 直接构造 HTTPSConnection
import http.client
c = http.client.HTTPSConnection("localhost\x00", timeout=5)
print("conn host:", repr(c.host), "tunnel:", repr(c._tunnel_host))
