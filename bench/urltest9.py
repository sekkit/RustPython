import urllib.request
url = "https://localhost\r\nX-injected: header\r\n:8080/test/?test=a"
try:
    urllib.request.urlopen(url, timeout=3)
except Exception:
    import traceback
    tb = traceback.format_exc()
    for line in tb.splitlines()[-14:]:
        print(line)
