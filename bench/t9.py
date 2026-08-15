import threading, time
results = []
def work(n):
    s = 0
    for i in range(1000):
        s += i * n
    results.append(s)
ts = [threading.Thread(target=work, args=(i,)) for i in range(4)]
[t.start() for t in ts]
[t.join() for t in ts]
print("threads ok, results:", sorted(results))
print("active count after join:", threading.active_count())
