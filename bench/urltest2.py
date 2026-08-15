import urllib.request
req = urllib.request.Request("http://localhost\x01/test/")
print("req.host =", repr(req.host))
req2 = urllib.request.Request("http://localhost\r\nX-injected: header\r\n:8080/test/?test=a")
print("req2.host =", repr(req2.host))
