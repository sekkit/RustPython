import sys
sys.path.insert(0, 'bench/sitepkg5')
# pytest 冒烟
try:
    import pytest
    print('pytest', pytest.__version__)
except Exception as e:
    print('pytest FAIL:', type(e).__name__, str(e)[:120])
