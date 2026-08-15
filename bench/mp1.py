import multiprocessing as mp
def f(q, n):
    q.put(n * n)
if __name__ == "__main__":
    ctx = mp.get_context("spawn")
    q = ctx.Queue()
    p = ctx.Process(target=f, args=(q, 7))
    p.start()
    print("result:", q.get())
    p.join()
    print("multiprocessing OK")
