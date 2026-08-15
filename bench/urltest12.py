import urllib.request
url = "https://localhost\r\nX-injected: header\r\n:8080/test/?test=a"
try:
    urllib.request.urlopen(url, timeout=3)
except Exception:
    import traceback
    for line in traceback.format_exc().splitlines()[-16:]:
        print(line)
