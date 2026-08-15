import sys
sys.path.insert(0, 'bench/sitepkg4')
mods = ['pytest', 'rich', 'yaml', 'PIL']
for m in mods:
    try:
        __import__(m)
        print(f"{m:8s} OK")
    except Exception as e:
        print(f"{m:8s} FAIL: {type(e).__name__}: {str(e)[:80]}")
