import sys
sys.path.insert(0, 'bench/sitepkg5')
# django 冒烟
try:
    import django
    print('django', django.get_version())
    from django.conf import settings
    settings.configure(DEBUG=True, SECRET_KEY='x', INSTALLED_APPS=['django.contrib.contenttypes'])
    django.setup()
    from django.urls import path
    from django.http import HttpResponse
    def home(request): return HttpResponse('ok')
    from django.urls.resolvers import URLPattern
    print('django routing ok')
except Exception as e:
    import traceback; traceback.print_exc()
    print('django FAIL:', type(e).__name__, str(e)[:100])
