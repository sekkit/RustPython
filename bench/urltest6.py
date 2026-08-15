import urllib.request
url = "https://localhost\r\nX-injected: header\r\n:8080/test/?test=a"
req = urllib.request.Request(url)
print("host:", repr(req.host))
print("tunnel:", repr(req._tunnel_host))
print("selector:", repr(req.selector))
