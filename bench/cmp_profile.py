import sys
import io
from difflib import unified_diff

which = sys.argv[1]  # 'profile' or 'cprofile'
import profile
import cProfile
import pstats
from test.profilee import testfunc, timer

profclass = cProfile.Profile if which == 'cprofile' else profile.Profile
prof = profclass(timer, 0.001)
start_timer = timer()
prof.runctx("testfunc()", globals(), locals())
elapsed = timer() - start_timer
print("elapsed:", elapsed)

path = r'C:\Dev2\luna-lang\RustPython\Lib\test\test_cprofile.py' if which == 'cprofile' else r'C:\Dev2\luna-lang\RustPython\Lib\test\test_profile.py'
src = open(path, encoding='utf-8').read()
tail = src.split('#--cut---')[1]
# skip the trailing dashes of the cut marker
tail = tail[tail.index('\n') + 1:]
head = tail[:tail.index('if __name__')]
ns = {}
exec(head, ns)
expected = ns['_ProfileOutput']

ok = elapsed == 1000
for method in ['print_stats', 'print_callers', 'print_callees']:
    s = io.StringIO()
    stats = pstats.Stats(prof, stream=s)
    stats.strip_dirs().sort_stats('stdname')
    getattr(stats, method)()
    output = s.getvalue().splitlines()
    mod_name = testfunc.__module__.rsplit('.', 1)[1]
    output = [line.rstrip() for line in output if mod_name in line]
    got = '\n'.join(output)
    exp = expected[method]
    if got == exp:
        print(method, ': MATCH')
    else:
        ok = False
        print(method, ': DIFF')
        print(''.join(unified_diff(exp.split('\n'), got.split('\n'), lineterm=''))[:2500])
print('ALL MATCH' if ok else 'DIFFS REMAIN')
