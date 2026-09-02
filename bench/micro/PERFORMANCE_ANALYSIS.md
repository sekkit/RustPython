# RustPython 性能基准分析（Controlled Experiments）

> 目标：全面诊断性能瓶颈根因，判定每个差距是"实现有问题（可修复）"还是
> "架构性分派地板（需大工程）"，不遗漏可节流项。

## 实验设计

用可控实验逐步分解调用链成本：
1. **fn_call 微基准**（`fn_call_bench.py`）：纯调用/带参/方法/递归/运算符/容器
2. **调用分解**（`call_decomp.py`）：纯调用 vs 3参数 vs 变长 vs 全局名 vs 递归
3. **多形态**（`poly_bench.py`）：单/多/超多形态，量化 specialization 缓存效率
4. **帧基准**（`frame_bench.py`）：深度递归、两层调用、单帧循环
5. **字节码指令成本**（`bytecode_bench.py`）：循环/加法/比较/属性/方法/下标/try/for-iter
6. **实例分配**（`alloc_bench.py`）：类实例/容器/闭包/生成器对象分配

## 数据（Windows x64，release 构建对比 CPython 3.12）

见 `PERF_SUMMARY.md` 完整表格。摘要：

| 组件 | RustPython | CPython | 差距 |
|---|---|---|---|
| 纯函数调用 | 220 ns | 53 ns | 4.2x |
| 参数打包（每多1参） | ~114 ns | ~37 ns | 3.0x |
| 两层调用/帧链接 | 376 ns | 68 ns | 5.5x |
| 单帧解释循环 | 1892 us | 565 us | 3.35x |
| 形态惩罚率（poly/mono） | 1.46x | 1.35x | 相近 |

**补充维度（详见各 *results.md）**：

| 维度 | 观测范围 | 最大差距 |
|---|---|---|
| 字节码指令（`bytecode_bench.py`） | while/for/compare/attr/method/subscr/try | call_method 4.7x |
| 实例分配（`alloc_bench.py`） | class/tuple/list/dict/closure/generator | **closure 7.1x** |

## 判定结论

**实现有问题（可节流）**：
1. `execute_call_vectorcall` 每次调用 `Vec::with_capacity` 堆分配参数数组——
   多数调用 ≤3 参数，可用栈缓冲；但需改 `VectorCallFunc` 签名（20+ 注册点），
   风险/收益比偏高，建议后续专项做
2. 参数打包路径 FuncArgs 堆分配（3.0x 差距）
3. **闭包创建 7.1x**（`alloc_bench.py`）——cell 环境分配+闭包对象，批量分配可节流
4. 普通容器分配 3.7-4.1x——PyInner/RwLock 簿记减负有空间

**架构性分派地板（需大工程）**：
1. 解释器主循环分派（3.35x）——CPython 的专家分派器优化多年
2. 帧创建/销毁（5.5x）——datastack 帧已降低成本，但对象化帧仍有开销
3. int 运算多态分派（3.1-3.4x）
4. 字节码指令 3.0-4.7x——与分派地板一致，无单个低风险节流项

**已确认高效的机制**：
- specialization 缓存（形态惩罚率与 CPython 相当）
- 容器操作（list/dict ~2.9x，接近地板）
- GC/内存分配（容器差距小于调用链）

## 推荐下一步（按收益排序）

1. **实例 freelist / 小对象复用**：减少 PyObject 分配（中收益、中风险）
2. **闭包/cell 批量分配**：7.1x 最大单项，改造成本低于 vectorcall 签名（中收益、中风险）
3. **vectorcall 参数栈缓冲**：改签名专项（中收益、高风险、需全量回归）
4. **解释器主循环快路径**：更多 `CallBuiltinFast`/`CallPyExactArgs` 类特化（持续收益）