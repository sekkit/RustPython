import sys
sys.path.insert(0, 'bench/sitepkg5')
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
with connection.schema_editor() as se:
    from django.contrib.auth.models import User
    se.create_model(User)
u = User.objects.create_user(username='alice', password='pw', email='a@b.c')
print("created:", u.username, u.id)
print("query:", User.objects.get(username='alice').email)
print("count:", User.objects.count())
print("django ORM + sqlite3 OK")
