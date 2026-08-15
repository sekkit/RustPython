import sqlite3
import django
from django.conf import settings
settings.configure(DEBUG=True, SECRET_KEY='x')
django.setup()
from django.db.backends.sqlite3._functions import register
conn = sqlite3.connect(":memory:")
try:
    register(conn)
    print("register OK")
except Exception as e:
    import traceback
    print("ERR:", type(e).__name__, e)
