# RustPython 性能优化候选调查报告

- 日期:2026-08-15
- 被测解释器:
  - RustPython:`target\release\rustpython.exe`,Python 3.14.0.alpha(RustPython 0.5.0, rustc 1.97.1, x64)
  - CPython 3.11.9(基准对照)
  - CPython 3.15.0b3(参照)
- 约束:本报告只读分析 + 测量,未修改任何 `src/`、`crates/` 源码;实验脚本位于 `bench\labs\perf\`。
- 说明:任务描述中提到的 `run_fast_interpreter`(~2275 行)与 `execute_interpreter_instruction`(~2481 行)在当前代码树中已不存在(已被重构)。当前对应结构为:
  - 主循环:`crates\vm\src\frame.rs:2967` `fn run(&mut self, vm)`(执行全部指令,无独立"快速"变体)
  - 指令分派:`crates\vm\src\frame.rs:3519` `fn execute_instruction(...)`(对约 227 个变体的巨型 `match`)

---

## 1. 当前差距数据

### 1.1 bench\bench.py(ops/s,越高越好;比值 = CPython3.11 ÷ RustPython)

| 基准 | RustPython | CPython 3.11 | CPython 3.15 | 差距(RP 慢倍数) |
|---|---|---|---|---|
| function call | 3,146,765 | 20,285,693 | 31,366,303 | **6.45x** |
| method call | 3,099,030 | 19,596,875 | 30,564,456 | **6.32x** |
| int arithmetic loop | 4,098,871 | 18,581,083 | 20,448,600 | **4.53x** |
| list index | 3,922,390 | 18,850,420 | 23,279,489 | 4.81x |
| dict get/set | 3,459,805 | 18,315,348 | 22,477,448 | 5.29x |
| string ops | 1,005,521 | 8,761,167 | 8,445,542 | **8.71x** |
| sort 500 floats | 43,449 | 65,173 | 53,232 | 1.50x |
| raise/catch exc | 639,039 | 4,184,868 | 4,693,588 | 6.55x |
| class instantiate | 2,466,382 | 14,857,849 | 18,722,017 | 6.02x |
| generator next | 528,804 | 4,233,368 | 8,634,824 | 8.01x |
| list comprehension | 90,872 | 341,215 | 465,318 | 3.75x |
| string format | 1,273,743 | 7,118,343 | 7,603,591 | 5.59x |

与任务背景中已知数据一致(call 6.4-7.0x、method 6.3-6.8x、字符串 8.7x、sort 1.5-1.6x、arith 4.5-4.8x)。注意 3.15 的对照显示差距并非由 CPython 3.11 偏慢造成——RustPython 相对 3.15 的差距更大。

### 1.2 bench\incremental.py(it/s 与每级增量成本 ns/迭代)

| 阶段 | RustPython it/s | CPython it/s | 差距 | RP 增量 ns | CP 增量 ns |
|---|---|---|---|---|---|
| for loop only | 12,119,454 | 90,151,545 | 7.44x | 82.5(基线) | 11.1(基线) |
| + load/store local | 10,327,669 | 66,911,414 | 6.48x | +14.3 | +3.9 |
| + int add | 7,219,885 | 33,167,459 | 4.59x | +41.7 | +15.2 |
| + call f(1,2) & use | 2,946,905 | 15,900,611 | 5.40x | +200.8 | +32.7 |
| + call f(1,2) discard | 3,269,795 | 19,671,873 | 6.02x | +167.3 | +20.7 |
| + method call __add__ | 2,281,852 | 14,997,450 | 6.57x | +132.4 | +15.8 |

要点:
- **空循环基线 82.5ns vs 11.1ns(7.4x)** 说明差距主要不是某个指令,而是解释器基础成本。
- **int add 增量 41.7ns vs 15.2ns(2.7x)** — 纯算术路径也有大差距(见 §3)。
- **调用增量 ~167-201ns vs ~21-33ns** — 调用路径虽已深度优化(tailcall、CallArgBuffer 零分配、datastack 帧),单次调用仍比 CPython 慢约 5-6x。
- **方法调用增量 132ns vs 16ns** — 属性查找 + 绑定方法 + 调用链路的组合成本。

### 1.3 bench\strbench.py(200,000 次,秒)

| 操作 | RustPython | CPython | 差距 |
|---|---|---|---|
| str.upper | 0.076 | 0.014 | 5.4x |
| str.lower | 0.076 | 0.014 | 5.4x |
| str.split | 0.368 | 0.049 | **7.5x** |
| str.join | 0.386 | 0.018 | **21.4x** |
| str.format | 0.248 | 0.032 | 7.8x |
| str.find | 0.070 | 0.015 | 4.7x |
| str.startswith | 0.060 | 0.014 | 4.3x |
| str slice | 0.061 | 0.010 | 6.1x |
| str.replace | 0.118 | 0.039 | 3.0x |
| len(str) | 0.030 | 0.005 | 6.0x |

**join 是全场最大的单项差距(21.4x)**,split/slice 也明显落后。

---

## 2. 小整数缓存现状(结论:已实现,与 CPython 完全一致)

### 2.1 现状(grep + 代码阅读)

RustPython **已有**与 CPython 相同的小整数缓存:

- `crates\vm\src\vm\context.rs:286` — `INT_CACHE_POOL_RANGE: RangeInclusive<i32> = (-5)..=256`(与 CPython 完全相同的区间)。
- `context.rs:324-332` — genesis 时预创建 262 个 `PyInt` 存入 `int_cache_pool`。
- `context.rs:435-443` `new_int()` / `context.rs:446-454` `new_bigint()` — 命中池范围时直接返回池内对象的 clone(仅一次原子 incref,无分配)。
- 字节码层面还有专门指令:`Instruction::LoadSmallInt`(`frame.rs:4288-4293`,"Push small integer (-5..=256) directly without constant table lookup"),编译端 `codegen\src\ir.rs:448-465` 会把小整数常量折叠为 `LoadSmallInt`(见 `compile.rs:18880-18899` 等处的折叠逻辑)。

### 2.2 实测(bench\labs\perf\int_cache_probe.py,RustPython vs CPython 3.11 输出完全一致)

- `[-6, 257]` 全区间逐值创建两次:262/264 个值 id 复用(仅 -6 与 257 不复用)——两解释器完全相同。
- `for i in range(5)` 两轮循环:每值 id 相同(5 个不同对象,两轮复用)。
- `id(1000+1) == id(1001)`:均为 True(算术结果走缓存)。
- 列表推导 `[i for i in range(5)]` 两轮:5 个不同 id,复用。

### 2.3 差距分析

小整数缓存**不是**性能差距来源——两解释器行为逐项一致。`arith 4.5x` 差距的真正原因是:
1. 循环变量 `i`(range 迭代器产出)与累加器 `s`(随迭代增长超出缓存区间)每次都是**新分配**的 PyInt,且 `PyInt` 内部是 `num-bigint::BigInt`(Vec 存储,`int.rs:82` 附近 `value: BigInt`),每次算术分配对象 + 分配内部 Vec,两次堆分配;CPython 的 `PyLong` 把数字内联在对象里,小值不产生第二次分配。
2. 见 §6 候选 2。

---

## 3. immortal 对象现状与可行性评估

### 3.1 现状

RustPython **没有真正的 PEP 683 immortal 概念**,但有三块相关基础设施:

1. **字节码层面有 opcode 但从未使用**:
   - `crates\compiler-core\src\bytecode\instruction.rs:655` — `LoadConstImmortal = 190` 已定义;`opcode_metadata.rs:426/459/792` 有元数据;`instruction.rs:1427` 断言其有常量参数。
   - **编译端从不发射 `LoadConstImmortal`**:全仓库 grep 仅 `crates\codegen\src\ir.rs:7566-7575`(单元测试)构造过它。
   - VM 端:`frame.rs:4246-4261` 处理 `LoadConst` 时自改写为 `LoadConstMortal`(`replace_op`),`LoadConstMortal | LoadConstImmortal` 两分支**逐字节相同**;`frame.rs:4248-4249` 注释明说 "RustPython does not currently distinguish immortal constants at runtime"。
2. **对象级只有一个 `LEAKED` 位**(`crates\common\src\refcount.rs:12,172-193`):
   - 仅用于 interned 字符串("leaked (interned). It will never be deallocated")。
   - `dec()`(`refcount.rs:157-170`)对 leaked 对象**仍然执行原子 `fetch_sub`**,只是返回 false 阻止释放——**并不跳过**递减,`inc()` 也照常递增。也就是说 interned 字符串仍付全量引用计数成本,只是永不释放。
3. **`sys._is_immortal` / `sys._is_interned` 未实现**:`crates\doc\src\data.inc.rs:31254-31255` 只有 CPython 抄来的文档字符串;运行时探测 `hasattr(sys, "_is_immortal")` 为 False(RustPython 与 CPython 3.11 均无,行为一致)。

### 3.2 可行性评估

| 项 | 评估 |
|---|---|
| 哪些类型可受益 | **代码对象常量**(每帧 `LOAD_CONST` 对常量做一次 clone/incref + 栈弹出时 decref,热循环里每次迭代 2 次原子 RMW);方法名/属性名 interned 字符串(当前仍付原子 inc/dec);小整数已走缓存(不受益)。None/True/False 已走 `LoadCommonConstant`/单例(受部分益)。 |
| 改动范围 | ① `crates\codegen` 在编译时对常量打 immortal 标记并发射 `LoadConstImmortal`(或运行时在 `PyCode` 创建时把常量整体 immortal 化,对应 `builtins\code.rs` 的常量元组);② `crates\common\src\refcount.rs` 增加 immortal 位并在 `inc`/`dec` 热路径跳过原子操作(`State::from_raw` 布局还有 3 个 flag 位,加一位可行);③ `crates\vm\src\frame.rs:4246-4261` 按 Immortal 分支真正区分。 |
| 预期收益 | 依赖 LOAD_CONST 密集的循环(如调用传参、常量字符串拼接)可省每次迭代 1-2 次 `lock xadd`(~10-20 cycle/次);对 call/字符串类基准整体约 5-15%。CPython 3.12+ 就是靠常量 immortal 把 LOAD_CONST 变成纯指针移动。 |
| 风险 | **中-高**:① 动态创建的 code object(`compile`/`exec`)常量若 immortal 化则永不释放,长驻嵌入场景会累积泄漏(CPython 接受此代价,RustPython 需评估);② 与 GC 遍历、`type_cache_clear`、QSBR 延迟回收的交互需审计;③ 引用计数正确性属核心安全区,回归面大。 |

**结论**:方向正确、基础设施(LoadConstImmortal opcode + VM 分支)已就位,但从"opcode 存在"到"安全跳过 inc/dec"之间隔着整个对象模型改动,应作为中期项目而非快速优化。

---

## 4. 字符串实现深挖(文件:行号级)

### 4.1 数据模型:每次结果都全新分配、无共享缓冲子串

- `crates\vm\src\builtins\str.rs:78-81` — `PyStr { data: StrData, hash }`,`StrData`(`crates\common\src\str.rs:127`)是**自有** `Box<Wtf8>`,不是 Cow/共享缓冲。
- `str.rs:493-505` `new_substr(&self, s: Wtf8Buf)` — 子串以**拷贝**的 Wtf8Buf 构造(partition/slice 走这里)。
- `str.rs:682-697` — hash 惰性计算(SENTINEL),创建时不付 hash 成本(这点与 CPython 相当)。
- `crates\common\src\str.rs:109-114` `PyKindStr` — 每次操作都要按数据内容**重判 kind**(ascii/utf8/wtf8 扫描)。

**对比 CPython**:CPython 3.12+ 的 split/slice 返回共享父缓冲的子串对象(immortal 化),3.11 虽拷贝,但按已知 kind 直接 memcpy,不重扫 kind。RustPython 每个结果都"拷贝 + 重判 kind + 新建对象(refcount/hash 字段)"。

### 4.2 具体发现

| 位置 | 问题 | 建议 |
|---|---|---|
| `anystr.rs:312-327` `py_join` | **无预扫描、无预分配**:循环 `push_str` 逐个追加,缓冲按需增长(小列表可多次 realloc);每个元素经 `PyResult<impl AnyStrWrapper + TryFromObject>` 协议转换(类型检查 + Result 展开)后才取内容 | 仿 CPython `unicode_join`:先一遍统计总长(对 exact str 元素零转换),单次 `with_capacity` + 批量 memcpy;对 list/tuple 的 exact-str 快路径直接取槽位指针,绕开迭代器协议 |
| `str.rs:1160-1177` `join` 入口 | `exactly_one` 快路径只覆盖 1 元素;通用路径 `zelf.as_wtf8().py_join(iter)?` 全走 4.2 第一行问题;最后 `vm.ctx.new_str(joined)` 再判一次 latin1 单字符缓存 | 合并到上面的预扫描实现 |
| `anystr.rs:169-200` `py_split` + `str.rs:802-844` `split` | 每个分片 `vm.ctx.new_str(s)` 全新分配(拷贝 + kind 重扫);`str.rs:826-833` UTF-8 路径用 `str::split` 后逐片转换 | 共享缓冲子串(§6 候选 4);至少对纯 ASCII 父串用字节切分 + 已知 Ascii kind 免重扫 |
| `str.rs:754-783` `lower`/`upper` | **无"结果相同则返回 self"快路径**:`"ABC".upper()` 每次分配新串;`to_ascii_uppercase()` 全量扫描 + 分配 | 先扫描是否已满足大小写;未变则返回原对象(CPython `unicode_upper` 正是如此)。对已全大写/全小写输入可省 100% 分配 |
| `str.rs:863-889` `strip` | 字符集判断用闭包逐字符 `chars.contains(c)` / memchr 单字节查找,无 `PyUnicode` 的快速路径 | 低优先 |
| `str.rs:1185-1204` `_find`/`char_range_bytes` | 查找路径 OK(有 byte↔char 索引),`find` 差距(4.7x)主要来自方法调用与解释器开销,不是算法 | 无 |
| `str.rs:493-505` + `str.rs:802-844` 共同点 | 所有分片/子串对象的创建都经过 `new_str`→`latin1_singleton_index` 检查(1 字节才命中)再 `into_ref` | 无 |

### 4.3 str.join 21.4x 差距归因(按影响排序)

1. 缺预扫描/预分配 + 逐元素 `push_str` 容量检查(realloc 对小列表影响有限,但每步都有分支)。
2. 逐元素协议转换:迭代器协议(对象创建)+ `TryFromObject` 类型检查 + `PyResult` 展开 + `as_ref().unwrap()` 二次解包——3 元素列表也要付 3 次。
3. 结果字符串创建:拷贝 + kind 重扫 + 新对象。
4. 方法调用/解释器基础开销(约 6x 的公共部分)。

---

## 5. dispatch 主循环固定开销分析

### 5.1 结构(当前代码)

- 主循环 `frame.rs:2967-3261` `run()`:单循环,无 tracing/无 tracing 双版本(CPython 有 `_PyEval_EvalFrameDefault` 内部 fast 路径分支)。
- 指令分派 `frame.rs:3519` `execute_instruction()`:`match instruction` 覆盖 **~227 个变体**(`instruction.rs` 枚举),LLVM 编译为跳转表 + 每臂内 `arg.get()`/`self.lasti()` 等。
- `flame_guard!` 宏(`crates\vm\src\macros.rs:206-211` 与 `crates\stdlib\src\macros.rs:1-7`):`#[cfg(feature = "flame-it")] let _guard = ::flame::start_guard(...)` — **非 flame-it 构建下整条语句被 cfg 剔除,零开销**。已确认 release 普通构建不付任何 profile 成本(任务背景的"flame 版慢 18 倍"正是该 feature 开启的代价,普通版无此问题)。

### 5.2 每次迭代的固定操作数(非 tracing、非 threading、x86-64 常规构建)

按 `run()` 源码逐行统计:

| 操作 | 位置 | 成本 |
|---|---|---|
| `lasti()` 原子 relaxed load | 2975 | 1 mov |
| `update_lasti(+1)` 原子 store | 2980 | 1 mov |
| `vm.use_tracing.get()`(Cell<bool>) + 分支 | 2986 | 1 mov + branch(短路,tracing 块内 `trace_is_set`/`read_op`/`locations.get` 均不执行) |
| `read_op(idx)`(Acquire load) | 3012 | 1 mov(x86 Acquire 无 fence) |
| `read_arg(idx)`(Relaxed load) | 3013 | 1 mov |
| `arg_state.extend`(移位/或) | 3013 | 2 ops |
| `op.cache_entries()` | 3015 | 常量/小查找 |
| prev_line 块:`matches!` + `is_instrumented()` + **`locations.get(idx)` 切片下标(含越界检查)+ `prev_line.set`** | 3026-3033 | 1 边界检查 + 1 load + 1 store + 分支 —— **无条件执行** |
| `vm.use_tracing.get()` + 分支(第二次) | 3035 | 1 mov + branch |
| `eval_breaker_tripped()`:信号字 relaxed load + test + branch | 3051 | 1 mov + branch(`vm\mod.rs:2727-2746`) |
| `lasti()` load(`lasti_before`) | 3090 | 1 mov |
| `execute_instruction` 跳转表分派 + 各臂工作 | 3091 | ~5-10 cycle 分派 |
| caches 检查:`lasti()` load + 比较 + 条件 store | 3093-3095 | 2 ops |
| `arg_state.reset()` | 3258 | 1 store |

固定开销合计:**约 8-10 次内存 load/store + 4-5 个分支 + 1 次切片越界检查 + 1 次跳转表分派**,估算约 15-35 cycle(~5-12ns @3GHz)。

### 5.3 结论

- **固定开销只占空循环 82.5ns 的小头**(估 10-15%)。空循环 7.4x 差距的主要构成是:指令体自身(FOR_ITER/POP_TOP/跳转/`lasti` 双读) + 值移动时的 PyObjectRef 引用计数原子操作。
- 栈值移动本身零成本:`PyStackRef`(`object\core.rs:2029-2149`)push 转移所有权、pop 还原所有权,均无 inc/dec;`push_borrowed`(`frame.rs:11153-11156`)已存在但**标注 `#[allow(dead_code)]`,当前无任何调用点**——借用栈槽的优化空间尚未启用。
- 引用计数原子出现在**值的复制点**:`LOAD_FAST`(locals clone)、`LOAD_CONST`(常量 clone)、`STORE_FAST`(替换旧值触发 decref)、容器写入等。每次 `lock xadd` ≈ 10-20 cycle,是"解释器基础成本"的主要物理来源。

### 5.4 可优化点列表(按预期收益排序,详见 §6)

1. **str.join 快速路径**(收益最大、风险最低)
2. **PyInt 小整数内联表示 / 算术快速路径**
3. **immortal 常量(LOAD_CONST 免引用计数)**
4. **upper/lower 等"结果未变返回 self" + ASCII 快速路径**
5. **主循环固定开销削减**(合并 use_tracing 双查、条件化 prev_line/locations、合并 lasti 读写)
6. **split/切片共享缓冲子串**
7. **推广 PyStackRef 借用引用**(`push_borrowed` 启用,LOAD_FAST/LOAD_CONST 免 incref)

---

## 6. 优化候选清单(按预期收益排序,前 5)

### 候选 1:str.join 专用快速路径

- **现状/依据**:`anystr.rs:312-327` + `str.rs:1160-1177`;strbench join 21.4x 差距(全场最大)。
- **方案**:仿 CPython `unicode_join`:① 对 `list`/`tuple` 的 exact-str 快路径(先验证全部元素为 exact str,零转换扫一遍求总长);② 单次 `with_capacity` 分配 + 批量字节拷贝;③ 通用路径保留但同样预扫描。
- **预期收益**:join 基准 5-20x;字符串密集型程序(格式化输出、模板、路径拼接)整体收益显著;测量上 strbench join 从 0.386s 有望降到 ~0.02-0.05s。
- **改动范围**:`crates\vm\src\anystr.rs`(`py_join`)、`crates\vm\src\builtins\str.rs`(`join` 入口)、可能涉及 `ArgIterable` 的 list/tuple 快路径访问。
- **风险**:低-中。需处理 str 子类元素(CPython 对 str 子类元素仍走通用路径)、错误消息一致性;不触及对象模型。

### 候选 2:PyInt 小整数算术快速路径(内联数字表示)

- **现状/依据**:`int.rs` 的 `value: BigInt`(`num-bigint`,Vec 存储);`number_op`(`int.rs:766-776`)每次 `a+b` 都产生新 BigInt(对象分配 + 内部 Vec 分配)。incremental "+ int add" 增量 41.7ns vs CP 15.2ns。
- **方案(两个层次)**:
  - 低风险版:在 `PyInt` 的 `number_op`/`add`/`sub`/`mul` 加 i64/i32 单字快速路径(操作数 `to_i64()` 成功且结果不溢出则直接用机器整数计算,避免 BigInt Vec 分配),失败回退 BigInt;
  - 高风险版:把 `PyInt` 内部改成 tagged union(内联 u64/i64 + BigInt 慢路径),对齐 CPython `PyLong` 的内联 `ob_digit`。
- **预期收益**:arith 类基准 2-4x(incremental int add 从 41.7ns 向 15-20ns 靠拢);所有整数密集型程序受益。
- **改动范围**:`crates\vm\src\builtins\int.rs`(+ 可能的 `common` 层 BigInt 封装)。
- **风险**:中-高(低风险版中;数据模型版高)。BigInt 语义(负数、符号)必须逐位保持;freelist(`INT_FREELIST`,`int.rs:60-99`)需同步适配。

### 候选 3:immortal 常量(LOAD_CONST 免引用计数)

- **现状/依据**:opcode 与 VM 分支已存在但从未启用(`instruction.rs:655`、`frame.rs:4246-4261`、`frame.rs:4248-4249` 注释);常量每 LOAD_CONST 一次原子 incref + 弹出 decref。
- **方案**:编译端(codegen)对常量发射 `LoadConstImmortal`(或 `PyCode` 创建时把常量元组元素打 immortal 标记)+ `refcount.rs` 增加 immortal 位并在 `inc`/`dec` 热路径跳过原子操作 + VM 分支真正区分。
- **预期收益**:LOAD_CONST 密集循环每次迭代省 1-2 次 `lock xadd`(~10-20 cycle);call/字符串/格式化类基准约 5-15%。
- **改动范围**:`crates\codegen\src\compile.rs`(常量发射)、`crates\common\src\refcount.rs`(状态位 + inc/dec 分支)、`crates\vm\src\frame.rs`(LoadConstImmortal 分支)、`crates\vm\src\builtins\code.rs`(常量生命周期)。
- **风险**:中-高。动态代码对象常量的内存累积(永不释放);与 GC/QSBR/缓存失效交互需审计;引用计数是核心安全区。

### 候选 4:upper/lower 等"结果未变返回 self" + ASCII 快速路径

- **现状/依据**:`str.rs:754-783`;`to_ascii_uppercase()` 无条件分配;strbench upper/lower 5.4x。
- **方案**:先扫描是否含需改变大小写的字符(全扫描但无分配);无变化则返回原对象(CPython 行为);ASCII 路径可用 u64 字块检测替换逐字节。
- **预期收益**:upper/lower 基准约 1.5-3x(对已全大写/全小写输入省全部分配;对混合输入省去扫描分配差);基准中的 `"abcdefgh".upper()` 输入非全大写,收益在 1.5-2x 左右。
- **改动范围**:`crates\vm\src\builtins\str.rs`(upper/lower/capitalize/title 等)。
- **风险**:低。注意语义:str 子类、lone surrogate 情况需保持;返回 self 时确保引用语义正确(返回 `zelf.to_owned()` 即可,仍是合法 str)。

### 候选 5:主循环固定开销削减

- **现状/依据**:§5.2 统计:每迭代 ~8-10 次 load/store、4-5 分支、1 次无条件 `locations.get` + `prev_line.set`(`frame.rs:3026-3033`)、2 次 `use_tracing.get`、`lasti` 三次读(2975/3090/3093)+ 一次写。
- **方案**:
  - 把 prev_line/locations 更新改为**仅在 tracing 开启或帧被观察时才维护**(CPython 的 f_lineno 是按需计算的;`prev_line` 只为 `f_lineno` 服务,`frame.rs:3017-3033` 注释自述代价"negligible",但每指令一次越界检查 + 写并非零);
  - 合并两处 `use_tracing.get()`(提前到一次,短路 opcode 事件检查);
  - `lasti` 三读一写压缩为尽量少的访问(如 `lasti_before` 直接用 2975 的值 +1 推导)。
- **预期收益**:每迭代省 3-6 cycle,约整体 2-5%;对 82.5ns 空循环可望降到 ~75-79ns。
- **改动范围**:`crates\vm\src\frame.rs`(run 循环内,纯局部重构,不动指令语义)。
- **风险**:低。f_lineno/断点/`sys._getframe` 行为需回归验证(`builtins\frame.rs` 读取 `prev_line` 的路径在 `frame.rs:508-525`)。

---

## 7. 附:实验脚本与复现

- `bench\labs\perf\int_cache_probe.py` — 小整数缓存探测(两解释器输出一致)。
- `bench\labs\perf\immortal_probe.py` — `sys._is_immortal`/`_is_interned` 可用性与常量身份探测。
- 基准:仓库自带 `bench\bench.py`、`bench\incremental.py`、`bench\strbench.py`。
- 复现命令:
  - RustPython:`$env:RUSTPYTHONPATH = "C:\Dev2\luna-lang\RustPython\Lib"; target\release\rustpython.exe bench\bench.py`
  - CPython:`python bench\bench.py`

## 8. 结论摘要

1. 小整数缓存已与 CPython 完全一致,不是差距来源。
2. immortal 只有"骨架"(opcode/VM 分支),真正跳过引用计数需要对象模型改动,收益中等、风险高,建议中期做。
3. 字符串:join(21.4x)与 split(7.5x)有明确的实现级低效点,是最值得先动手的区域;upper/lower 有低风险快路径。
4. dispatch:固定开销(15-35 cycle/迭代)只占空循环的一小部分;`flame_guard!` 确认 cfg 剔除零开销;更深的差距来自指令体与值复制点的原子引用计数(`lock xadd`),immortal 与借用栈槽是削减它的两条主线。
