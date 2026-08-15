# 单进程内多个独立 VM(Rust 库形态独有能力),这里用多进程模拟脚本侧隔离成本
import multiprocessing, time
def spin(n):
    s = 0
    for i in range(n):
        s += i ^ (i << 1)
    return s
if __name__ == "__main__":
    N = 2_000_000
    t0 = time.perf_counter()
    p1 = multiprocessing.Process(target=spin, args=(N*4,)); p1.start(); p1.join()
    print(f"1 proc x 4N: {time.perf_counter()-t0:.2f}s")
