import urllib.request
req = urllib.request.Request("https://localhost\x00/test/")
print("tunnel_host:", repr(req._tunnel_host))
print("host:", repr(req.host))
