import threading, time
def spin(n):
    s = 0
    for i in range(n):
        s += i ^ (i << 1)
    return s
N = 2_000_000
# serial
t0 = time.perf_counter()
spin(N); spin(N); spin(N); spin(N)
serial = time.perf_counter() - t0
# threaded
t0 = time.perf_counter()
ts = [threading.Thread(target=spin, args=(N,)) for _ in range(4)]
[t.start() for t in ts]
[t.join() for t in ts]
par = time.perf_counter() - t0
print(f"serial: {serial:.2f}s  threaded: {par:.2f}s  par/serial = {par/serial:.2f}")
