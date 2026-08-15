import urllib.request
url = "https://localhost\r\nX-injected: header\r\n:8080/test/?test=a"
try:
    urllib.request.urlopen(url, timeout=3)
    print("NO RAISE?!")
except Exception as e:
    print("raised:", type(e).__name__, str(e)[:70])
