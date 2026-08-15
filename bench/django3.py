import sqlite3, traceback
import django
from django.conf import settings
settings.configure(
    DEBUG=True, SECRET_KEY='x',
    INSTALLED_APPS=['django.contrib.contenttypes', 'django.contrib.auth'],
    DATABASES={'default': {'ENGINE': 'django.db.backends.sqlite3', 'NAME': ':memory:'}},
)
django.setup()
from django.apps import apps
apps.populate(['django.contrib.contenttypes', 'django.contrib.auth'])
from django.db import connection
# 手动触发连接
try:
    connection.ensure_connection()
    print("ensure_connection OK")
except Exception:
    traceback.print_exc()
