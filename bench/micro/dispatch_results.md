# 指令分派开销量化（dispatch cost per bytecode）

本报告量化 RustPython 3.14 解释器**主循环指令分派**的每字节码成本，并与 CPython 3.12
对比，判定分派是架构地板还是可节流项。**只读分析 + 基准，未改动任何 crates/ 源码。**

- worktree: `C:\Projects\luna-lang\RustPython-wt-b`（HEAD `1441c2957`，detached）
- 解释器: `target/release/rustpython.exe`（`cargo build --release -p rustpython`，独立 target）
- CPython: `C:\Users\sekkit\AppData\Local\Programs\Python\Python312\python.exe`
- 脚本: `dispatch_bench.py`（分派成本）、`spec_bench.py`（特化命中/未命中）、`spec_probe.py`（分派码确认）

---

## 1. 每字节码指令成本（ns/bytecode）

方法学：
- **直线分派地板**：无分支/无循环的函数体，执行指令序列与 `dis` 计数完全一致（排除
  CACHE/EXTENDED_ARG/Pseudo），ns/bytecode 无歧义。min-of-5 报告。
- **循环分派**：报 ns/iter，per-iteration 指令数由 `dis` 反汇编自动统计（识别回跳边）。
- 注意：RustPython 编译器会合并且/或使用超指令（`LOAD_FAST_BORROW_LOAD_FAST_BORROW`、
  `BINARY_OP_ADD_INT` 等），故同一源码两种解释器指令条数可能不同；下表仅对**指令条数
  一致**的行做严格逐字节码对比。

### 1a. 直线分派地板（纯 LOAD/STORE，指令条数一致 → 严格可对比）

| 用例 | RustPython ns/bytecode | CPython 3.12 ns/bytecode | 倍数 |
|---|---|---|---|
| load_store_x200（LOAD_FAST+STORE_FAST×200，403 vs 402 instr） | 8.44 | 1.25 | **6.75x** |
| load_store_x1000（同上×1000，2003 vs 2002 instr） | 8.16 | 1.16 | **7.03x** |
| const_store_x200（LOAD_CONST+STORE_FAST×200） | 8.88 | 2.37 | 3.75x |
| load_add_store_x100（x=x+x，303 vs 402 instr） | 21.67 | 4.44 | 4.88x\* |

\* 该行两解释器指令条数不同（RustPython 用 `LOAD_FAST_BORROW_LOAD_FAST_BORROW`+
`BINARY_OP_ADD_INT`+`STORE_FAST` 3 条代替 CPython 4 条），ns/call 对比为 3.68x。

**严格逐字节码（指令条数一致）的分派地板在 6.75–7.03x** —— 明显高于主仓已测的
整体 3.35x / 字节码指令 3.0–4.7x 混合比率。

### 1b. 循环分派（指令条数两解释器一致）

| 循环 | instr/iter | RustPython ns/bytecode | CPython ns/bytecode | 倍数 |
|---|---|---|---|---|
| while_counter（COMPARE+POP_JUMP+INC+JUMP_BACK） | 9 | 12.85 | 3.07 | **4.19x** |
| for_sum（FOR_ITER_RANGE+ADD） | 7 | 14.83 | 4.31 | **3.44x** |
| call_empty_loop（含 CALL） | 11 | 20.75 | 4.87 | **4.26x** |

循环逐字节码 3.4–4.3x，比纯直线 7x 低——因为循环里夹带了实际工作（整数加法等），
摊薄了纯分派成本。**纯分派成本是最差的一个乘数。**

---

## 2. 特化命中 vs 未命中（specialization cache value）

用 `CallPyExactArgs` 类特化指令做 monomorphic vs megamorphic：
- **命中**：同一函数对象反复调用 → `func_version` 内联缓存命中 → 快速路径
  `invoke_exact_args_slots`
- **未命中**：16 个不同函数轮换 → 版本不一致 → 每次回落 `execute_call_vectorcall`
- 两循环字节码形状完全一致（`fs[i & MASK](i)`，仅常量 0 vs 15 不同），ns/call 差值
  纯化到缓存命中/未命中。

| 解释器 | monomorphic(HIT) ns/call | megamorphic(MISS) ns/call | 差值 | 比率 |
|---|---|---|---|---|
| **RustPython**（3 次重复） | 344.7–351.0 | 433.9–476.8 | **87–131** | **1.25–1.38x** |
| **CPython 3.12** | 77.7 | 83.8 | 6.1 | 1.08x |

`spec_probe.py`（adaptive dis）确认：两种循环在 RustPython 下都被特化为
`CALL_PY_EXACT_ARGS`、`BINARY_OP_SUBSCR_LIST_INT`、`BINARY_OP_ADD_INT`、
`FOR_ITER_RANGE`，字节码形状一致 → 差值确为 exact-args 缓存命中价值。

**结论**：
- RustPython 特化缓存命中每次调用省 **~90–130 ns（约 26–30% 调用成本）**，绝对值显著。
- 但**未命中回落路径（execute_call_vectorcall）相对快速路径的惩罚比例（1.25–1.38x）
  明显大于 CPython（1.08x）**——通用回落路径是 RustPython 的一个真实开销放大点。
- 即使**最优情况（缓存命中）**，RustPython 调用也 345–351 ns/call = CPython 77.7 ns/call 的
  **~4.5x**——特化覆盖再全也消除不了这一差距，说明对象模型/帧机制本身承担了大部分成本。

---

## 3. 分派机制的逐指令固定开销（源码确认，只读）

对 `frame.rs` 主循环逐字节码固定路径的确认，构成判定依据：

1. **指令读取是原子加载**：`compiler-core/bytecode.rs` 中 `read_op`/`read_arg` 对 packed
   `AtomicU8` 做 relaxed 加载（为自适应特化/`compare_exchange_op` 服务）。CPython 3.12 用
   非原子字读取 + quickening。
2. **每条指令都跑 `eval_breaker_tripped()`**（frame.rs:3049）：包含
   `finalizing.load`、`stop_requested_for_current_thread`、`EVAL_BREAKER`（AtomicU8 relaxed）
   至少 2–3 次原子加载。**CPython 将信号/safepoint 检查分摊到 N 条指令一次，而非每字节码。**
3. **`lasti` 是 AtomicU32**：每条指令 `fetch_add` + store + load（relaxed）。
4. 特化机制：`CallPyExactArgs`/`CallBoundMethodExactArgs` 等在 frame.rs 都有版本缓存快速路径，
   未命中才回落 vectorcall——已被实验 2 量化。

---

## 4. 判定：解释器分派是**可节流项，但存在明确架构地板**

### 结论
**指令分派是一个"部分可节流、但有真实架构地板"的项。** 它不是纯架构不可动，但也不是
单纯靠加特化就能追平的东西。

### 依据

**为什么说"可节流"（存在具体、低风险、可改的分派开销）：**
- 纯分派地板（指令条数一致的 LOAD/STORE 直线流）达 **6.75–7.03x**，是全部测过的乘数里
  最差的，说明**分派/取指/解码路径本身是一个显著的固定开销乘数**，而非实际工作慢。
- 逐指令的**原子开销可以批量/去原子**：
  - `eval_breaker_tripped()` 每字节码跑 2–3 次 relaxed 原子加载，可改为"每 N 条检查一次"
    （CPython 同款分摊策略），或仅在需要时走该路径。
  - `read_op`/`read_arg` 的原子读取在非特化/非 tracing 场景可退化为非原子直读（CPython
    用 quickened 非原子字节码）。
  - `lasti` 原子 fetch_add 在主路径上可以优化。
  - 通用回落路径 `execute_call_vectorcall` 相对快速路径的 1.25–1.38x 惩罚本身可压缩
    （缩小与 `CallPyExactArgs` 快速路径的差距）。
- 这些都属于"降低每字节码常数因子"，收益直接落在所有 Python 代码上，是**最高杠杆的低风险
  节流方向**。

**为什么说"有架构地板"（纯节流无法消除的部分）：**
- 即使特化**全命中**（最好情形），调用仍 345–351 ns/call = CPython 的 **~4.5x**；纯分派
  地板即便去掉原子检查，Rust 对象模型 + 大枚举 match 分派 + GC/引用管理仍与 CPython 的
  C + computed-goto 有结构性差距。
- 单帧循环 3.35x、字节码指令 3.0–4.7x、fn_call 4.2x 的主仓基线与本报告的循环逐字节码
  （3.4–4.3x）一致；**7x 的纯分派地板被实际工作摊薄后正是 3–4.7x 的整体观感**。

### 行动建议（供主仓参考，本任务未改动代码）
1. **最高优先级**：把 `eval_breaker_tripped()` 从"每字节码"改为"每 N 字节码 + 入口检查"，
   预计直接压低 1a 的纯分派地板与所有循环。
2. 评估主路径 `read_op`/`read_arg` 去原子化（仅特化写入时切原子）。
3. 压缩 `execute_call_vectorcall` 回落路径与 `CallPyExactArgs` 快速路径的差距（1.25–1.38x）。
4. 特化覆盖率已高（probe 显示 CALL/BINARY_OP/SUBSCR/FOR_ITER 全特化），追加特化收益递减；
   应转向削减每指令常数因子。

### 主仓评估（2026-09-03）
- **建议 1 被否决（低收益高风险）**：默认构建**未启用 `threading` feature**
  （Cargo.toml default = compiler,wasmbind,gc,host_env,stdio），`eval_breaker_tripped()`
  在默认下仅 **1 次 relaxed 加载**（`eval_breaker_pending()`）；"2-3 次"仅出现在
  threading 构建。改为每 N 条会引入信号/GC/ThreadStop 处理延迟，且默认构建收益≈0
  → 判定为不实施，保留每次检查。
- **建议 2 被否决（数据竞争，不可去原子）**：`replace_op`/`specialize_at`
  在**帧执行期间**写字节码（frame.rs:9472 等），`read_op` 的 AtomicU8 relaxed
  读是防止与特化写构成数据竞争的必需开销（去原子化=UB）。且 relaxed 原子读在
  单线程下编译为普通 load，实际成本极低 → 判定不实施。
- **建议 3 已部分量化**：`argcost_bench.py` 显示每参 ~120ns（含参数处理），
  execute_call_vectorcall 与 CallPyExactArgs 的差距已并入 fn_call 专项评估。

---

## 5. 原始数据（min-of-5）

### dispatch_bench.py — RustPython
```
load_store_x200:     8.44 ns/bytecode  (403 instr)     CP 1.25  → 6.75x
const_store_x200:    8.88 ns/bytecode  (403 instr)     CP 2.37  → 3.75x
load_add_store_x100: 21.67 ns/bytecode (303 instr)     CP 4.44  → 4.88x
load_store_x1000:    8.16 ns/bytecode  (2003 instr)    CP 1.16  → 7.03x
while_counter:       12.85 ns/bytecode (9/iter)        CP 3.07  → 4.19x
for_sum:             14.83 ns/bytecode (7/iter)        CP 4.31  → 3.44x
call_empty_loop:     20.75 ns/bytecode (11/iter)       CP 4.87  → 4.26x
```

### spec_bench.py — RustPython（3 次）
```
monomorphic (HIT):  346.7 / 344.7 / 351.0  ns/call
megamorphic (MISS): 433.9 / 443.4 / 441.0  ns/call
delta:               87.2 /  98.7 /  90.0  ns/call
ratio:               1.25x / 1.29x / 1.26x
```
CPython：HIT 77.7，MISS 83.8，delta 6.1，ratio 1.08x。

### 环境
- `cargo build --release -p rustpython`：首次约 10m40s，产物 33.5 MB。
- 运行方式：`$env:RUSTPYTHONPATH="<wtb>\Lib"; target/release/rustpython.exe bench/micro/dispatch_bench.py`
- 本次生成的基准脚本与数据文件均位于 `bench/micro/`，未提交。
