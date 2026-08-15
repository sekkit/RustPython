import sys
sys.path.insert(0, 'bench/sitepkg4')
for m in ['yaml', 'rich', 'markdown_it']:
    try:
        __import__(m)
        print(f"{m:12s} OK")
    except Exception as e:
        print(f"{m:12s} FAIL: {type(e).__name__}: {str(e)[:90]}")
# 功能测试
import yaml
print('yaml dump:', yaml.dump({'a': [1,2,3]}).strip())
from rich.console import Console
c = Console(force_terminal=True, width=40)
from io import StringIO
buf = StringIO()
c2 = Console(file=buf, force_terminal=True, width=40)
c2.print("[bold red]hi[/bold red]")
print('rich out:', buf.getvalue()[:50])
