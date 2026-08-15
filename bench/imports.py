"""Startup + import benchmark: measures interpreter boot and stdlib import costs."""
import time, sys, subprocess

IMPL = sys.implementation.name

# measured inside a fresh process: cold import time for key modules
COLD = """import time
t0 = time.perf_counter()
import %s
print("import %s: %.1f ms" % (time.perf_counter() - t0, 1_000))
"""

mods = ["json", "re", "collections", "itertools", "math", "random", "logging", "argparse", "unittest", "datetime"]

if IMPL == "rustpython":
    exe = [r"target\release\rustpython.exe"]
else:
    exe = ["python"]

import os
env = dict(os.environ)
env["RUSTPYTHONPATH"] = os.path.join(os.getcwd(), "Lib")

print(f"=== cold import times on {IMPL} ===")
for m in mods:
    code = f"""
import time
t0 = time.perf_counter()
import {m}
dt = (time.perf_counter() - t0) * 1000
print(f"{m:12s} {{dt:8.1f}} ms")
"""
    try:
        r = subprocess.run(exe + ["-c", code], capture_output=True, text=True, env=env, timeout=60)
        out = r.stdout.strip().splitlines()
        print(out[-1] if out else f"{m:12s} FAILED: {r.stderr.strip()[:80]}")
    except Exception as e:
        print(f"{m:12s} ERROR {e}")
