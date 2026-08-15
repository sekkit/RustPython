import http.client
c = http.client.HTTPSConnection("localhost\r\nX-injected: header\r\n", 8080)
print("constructed, host =", repr(c.host))
