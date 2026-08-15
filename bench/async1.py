import asyncio, time

async def worker(name, delay):
    await asyncio.sleep(delay)
    return f"{name}-done"

async def main():
    t0 = time.perf_counter()
    results = await asyncio.gather(
        worker("a", 0.1), worker("b", 0.05), worker("c", 0.2)
    )
    dt = time.perf_counter() - t0
    print("gather:", results, f"in {dt:.2f}s (期望 ~0.2s 并发)")

    # 任务取消
    async def slow():
        try:
            await asyncio.sleep(10)
        except asyncio.CancelledError:
            return "cancelled"
    task = asyncio.create_task(slow())
    await asyncio.sleep(0.05)
    task.cancel()
    print("cancel:", await task)

    # 子进程/线程执行器
    loop = asyncio.get_running_loop()
    r = await loop.run_in_executor(None, lambda: 40 + 2)
    print("executor:", r)

asyncio.run(main())
print("asyncio OK")
