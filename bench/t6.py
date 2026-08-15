import ssl, urllib.request
ctx = ssl.create_default_context()
try:
    r = urllib.request.urlopen("https://www.baidu.com", timeout=30, context=ctx)
    print("status", r.status)
except Exception as e:
    print("ERR:", type(e).__name__, str(e)[:120])
