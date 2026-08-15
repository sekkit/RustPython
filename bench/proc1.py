import subprocess, os, tempfile
# subprocess 管道
r = subprocess.run(["python", "-c", "print('hi from child')"], capture_output=True, text=True, timeout=30)
print("subprocess:", r.stdout.strip())
# 大文件写入/读取
with tempfile.NamedTemporaryFile(delete=False, suffix=".bin") as f:
    data = os.urandom(5_000_000)
    f.write(data)
    path = f.name
with open(path, "rb") as f:
    back = f.read()
print("bigfile 5MB:", "OK" if back == data else "MISMATCH")
os.unlink(path)
