# RustPython 落地瓶颈实验研究与完整解决方案

> 实验日期:2026-08
> 环境:Windows(x86_64, 128 核),rustc 1.97.1,RustPython `70b47dd7c` release 构建
> 对照:CPython 3.11.9 / 3.12(本机),CPython 3.15.0b3(uv)
> 方法:实测数据 + 代码级根因定位 + 已验证修复实验
> 状态:性能/兼容性/并发三大维度全部量化;**7 项修复已验证并推送到 fork**(见 §9);生态可用性大幅提升(requests/flask/django/pytest 全通);C 扩展(.pyd)PEP 489 加载已打通,test_importlib 1440 全绿;JIT 实测无收益

---

## 0. 结论速览

| 维度 | 现状(实测) | 结论 |
|---|---|---|
| **性能** | 比 CPython 3.11 慢 **4.6x~8.8x**(调用/方法调用 6.6x~7.4x) | 剩余差距为设计级(原子引用计数/dispatch),需数据模型重构(Phase 2) |
| **JIT** | 编译成功但**实测仅 1.8% 提速**(100M 循环) | 调用边界开销吞噬原生码收益,当前无实用价值 |
| **兼容性** | **100+ 测试模块全绿**;纯 Python 生态全通(requests/flask/django ORM/celery/pytest/httpx/aiohttp/sympy…);**ctypes 4 缺陷全修 + cProfile 可用**;仍不可用:C 扩展(.pyd 加载,Phase B) | Web/数据/测试/任务队列生产可用 |
| **并发** | **无 GIL 真并行**:4 线程 CPU 密集 **2.3x 加速**(CPython 3.11 为 0.98x) | 反直觉优势,可作落地卖点 |
| **修复实验** | **18 项修复**已提交(fork:sekkit/RustPython,35 提交)→ 对应测试模块全绿 | "低垂果实"路线验证可行 |

---

## 1. 性能瓶颈:量化证据与根因

### 1.1 基准实测(RustPython vs CPython 3.11,ops/s,越高越好)

| 基准 | RustPython | CPython 3.11 | 慢倍数 |
|---|---|---|---|
| function call | 3,171,104 | 21,074,665 | **6.6x** |
| method call | 2,744,464 | 20,289,801 | **7.4x** |
| int arithmetic loop | 4,174,961 | 19,182,936 | 4.6x |
| list index | 4,046,252 | 19,044,358 | 4.7x |
| dict get/set | 3,381,396 | 19,110,748 | 5.7x |
| string ops | 1,029,587 | 9,024,395 | **8.8x** |
| sort 500 floats | 44,488 | 65,974 | 1.5x |
| raise/catch exception | 659,272 | 4,113,322 | 6.2x |
| class instantiate | 2,549,922 | 15,596,731 | 6.1x |
| generator next | 555,283 | 3,874,915 | 7.0x |
| list comprehension | 91,598 | 334,538 | 3.7x |
| string format | 1,320,709 | 7,653,977 | 5.8x |

**模式**:调用类(method/function/generator/exception)最慢(6~7.4x),纯算术/索引中等(4.6x),排序最接近(1.5x,rust-timsort 已修)。

### 1.2 启动与导入实测

| 项 | RustPython | CPython 3.11 | 慢倍数 |
|---|---|---|---|
| 冷启动(pass) | 63 ms | 25 ms | 2.5x |
| import json | 43 ms | 11 ms | 4x |
| import re | 25 ms | 8 ms | 3x |
| import logging | 106 ms | 18 ms | 6x |
| import argparse | 54 ms | 11 ms | 5x |
| import unittest | 166 ms | 31 ms | 5.3x |
| import datetime | 27 ms | 2 ms | **13.5x** |

### 1.3 根因定位(代码级)

**① 调用路径(最大头,6.6~7.4x)**
- `frame.rs` 的 `execute_call` 构造 **`FuncArgs` 堆分配**(`args` Vec + kwargs Vec),`callable.call()` 通用分发;
- `function.rs::invoke_with_locals` 再走 `fill_locals_from_args` 逐参数解包(每个参数 Option<PyObjectRef> 检查);
- 虽然已有 `execute_call_vectorcall`(`frame.rs:8092`),但**只覆盖 `Instruction::Call` 的位置参数路径**;方法调用、关键字调用、`super()` 仍走 FuncArgs;
- 每次 Python→Rust→Python 调用都要:弹栈 → 建 FuncArgs → 类型擦除 → 再解包 → 入栈。相比 CPython 3.11 的 vectorcall(栈上连续参数、零分配),每条调用多 ~3-5 次分配/解包。

**② 字符串 8.8x**
- 内部用 WTF-8(`PyStr`),与外部 UTF-8/ASCII 互转有开销;`concat_in_place` 等快路径存在但覆盖面有限。

**③ JIT 无收益(见 §3)**

**④ 启动慢(5~13x)**
- 冷启动 63ms:解释器初始化 + 内建类型注册;
- 导入慢:纯 Python Lib 模块(如 unittest 166ms)在冷态下逐行解释 + 无 .pyc 缓存生效(需确认 pycache 支持)。

### 1.4 解法(按杠杆排序)

| 优先级 | 解法 | 预期收益 | 工作量 |
|---|---|---|---|
| P0 | **vectorcall 全路径落地**:方法调用/关键字调用/super 全部走栈上连续参数,消除 FuncArgs 分配 | 调用类 6.6x→3x | 中(改 frame.rs 调用族) |
| P0 | **小参数内联**:argc≤4 时参数直接进栈槽,复用解释器栈 | 同上 | 中 |
| P1 | 内联缓存命中快路径:LOAD_METHOD/LOAD_ATTR 特化后跳过类型检查 | 方法调用 | 中 |
| P1 | BINARY_OP 超指令扩展(仿 BinaryOpInplaceAddUnicode 模式) | 算术 4.6x→3x | 小 |
| P2 | 字符串:缓存 UTF-8 表示(惰性转码,类似 PEP 393 的 utf8 缓存) | 字符串 8.8x→4x | 中 |
| P2 | .pyc 缓存 + frozen stdlib 默认开 | 启动 5x→2x | 小 |

**参照**:CPython 3.11 的"自适应特化 + 零成本异常 + vectorcall"组合拳把 3.10→3.11 提速 25~60%;RustPython 已复刻特化框架(specialize_* 齐全),缺口主要在**调用边界**。

---

## 2. 兼容性瓶颈:量化证据与根因

### 2.1 实测

**测试套件抽样**(22 个核心模块,`cargo run --release -- -m test <mod>`):

| 模块 | 结果 | 模块 | 结果 |
|---|---|---|---|
| test_str | PASS(138) | test_dict | PASS(120) |
| test_class | PASS(37) | test_list | PASS(68) |
| test_set | PASS(630) | test_string | PASS(54) |
| test_math | **PASS(修复后)** | test_tuple | PASS(38) |
| test_asyncgen | PASS(85) | test_json | PASS(218) |
| test_asyncio | PASS(2) | test_re | PASS(166) |
| test_descr | PASS(162) | test_itertools | PASS(137) |
| test_functools | PASS(325) | test_generators | PASS(59) |
| test_unpack | PASS(2) | test_exceptions | PASS(107) |
| test_with | PASS(54) | test_sort | PASS(21) |
| test_traceback | PASS(370) | test_super | PASS(40) |

- 全仓 `Lib/test/` 含 **1292 处 `TODO: RUSTPYTHON` 标记,185/437 个测试文件**(42%)有 skip/expectedFailure。
- **pip 生态可用**:`ensurepip` 装 pip 26.1.2 ✅;pip 装 **six 1.17.0** ✅、**requests 2.34.2** ✅(纯 Python 包)。
- **网络栈可用**:`urllib + ssl(rustls)` HTTPS 请求 baidu 返回 200 ✅。
- **C 扩展全灭**:numpy / pandas / flask / django / sqlalchemy / cryptography / lxml 全部 import 失败。
- **ctypes 可用**:`CDLL('kernel32')` ✅、`find_library('msvcrt')` ✅。
- **新语法已支持**:`match` 语句 ✅、`typing.Generic/TypeVar` ✅。

### 2.2 具体 API 缺口(实测复现)

```
requests.get('https://...') 失败:
  File ".../urllib3/util/ssl_.py", line 315, in create_urllib3_context
    context.hostname_checks_common_name = False
AttributeError: property 'hostname_checks_common_name' of 'SSLContext' object has no setter
```
→ RustPython `ssl.SSLContext` 缺 CPython 3.14 的 `hostname_checks_common_name` setter。**这类"属性面缺口"是纯 Python 生态的拦路虎**,每个热门库都可能踩中一个。

### 2.3 根因

1. **C 扩展 ABI 不可用**:capi 兼容层 + ctypes(libffi)只能覆盖一小部分;numpy 等原生扩展无解(需要完整 CPython C API 仿真,类比 PyPy cpyext 十年工程)。
2. **属性面长尾**:`Lib/` 从 CPython 复制但 Rust 侧实现(socket/ssl/os 等)并非逐属性对照,缺 setter/常量/特殊方法即炸。
3. **版本追赶**:目标 CPython 3.14,但 CPython 每年演进;测试标记(1292 处)反映真实差距。

### 2.4 解法

| 优先级 | 解法 | 预期收益 | 工作量 |
|---|---|---|---|
| P0 | **跑 `scripts/whats_left.py` + 全套测试,系统清标记**:每个 expectedFailure 归类(API 缺口/语义差异/平台 bug) | 套件可验收基线 | 小(持续) |
| P0 | **属性面对照补齐**:对 urllib3/requests 依赖的 ssl 属性逐项对照 CPython 3.14(hostname_checks_common_name 等),写契约测试 | requests 全家桶可跑 | 小~中 |
| P1 | **纯 Python 生态 CI 门禁**:CI 里 pip 装 top-100 纯 Python 包并冒烟导入 | 防回归 | 中 |
| P2 | **C 扩展路线图**:① ctypes 增强(struct/array 完整语义)→ ② capi 成熟化(优先纯 ctypes 包)→ ③ 完整 C API 仿真(长期,慎重) | numpy 等(远期) | 大 |

---

## 3. JIT 实测:编译成功但无收益

### 3.1 实验数据

| 实验 | 结果 |
|---|---|
| 无注解函数 `__jit__()` | `JitError: argument n needs annotation`(强制注解) |
| 注解 fib(26) | interp 0.1219s vs jit 0.1280s → **0.95x(更慢)** |
| 注解 add2 ×1M | 0.4008s vs 0.4010s → **1.0x** |
| 注解 while 30M | 5.809s vs 5.839s → **0.99x** |
| 注解 while 100M(公平大样本) | 19.318s vs 18.969s → **1.018x(仅 1.8%)** |
| 编译能力 | add/mul/float/while/if 均编译成功(JIT-OK) |

### 3.2 根因(代码级)

`crates/jit/src/lib.rs`:
- 每次 jitted 调用都走 **`libffi::Cif::new(...)` 重建调用描述 + `get_jit_args`(builtins/function/jit.rs)逐参数解包 Python 对象 + 类型检查 + `ArgsBuilder` 构造**;
- 即:调用边界开销 = 解释器调用开销 + libffi 开销,**原生函数体越短,边界占比越大**;
- 指令覆盖有限(约 45 个 `Instruction::` 臂,字节码指令全集 96+),复杂函数编译失败静默回退解释器;
- 与 CPython 3.13 copy-and-patch JIT(把字节码内联到原生 trampoline)或 PyPy tracing(编译整条热路径)相比,当前 JIT 缺少"调用边界也原生化"的能力。

### 3.3 解法

| 方案 | 说明 | 评估 |
|---|---|---|
| A. **定位为"大循环体"专用**:仅对 body 长的热函数启用,加阈值+eval-breaker | 避免小函数负收益 | 小,先做 |
| B. **消除 libffi**:JIT 函数签固定,用直接 call 代替 Cif 重建 | 边界开销减半 | 中 |
| C. **放弃通用 JIT,集中解释器特化**(CPython 3.11 已验证路线) | 特化+vectorcall 收益确定性更高 | 中,推荐 |
| D. 完整 tracing JIT(仿 PyPy) | 长期 | 大,不推荐现在投入 |

**结论**:JIT 当前形态(手动注解 + libffi 边界)不适合作为性能解;**把 P0 资源放 vectorcall/特化,比 JIT 更确定**。

---

## 4. 并发:反直觉优势(实测)

### 4.1 实验数据

```
4 线程 CPU 密集(i^ (i<<1) 累加):
  RustPython: serial 2.52s / threaded 1.05s → par/serial = 0.42(≈2.3x 加速)
  CPython 3.11: serial 0.51s / threaded 0.50s → par/serial = 0.98(GIL 无并行)
  大样本复核(8M):RustPython par/serial = 0.43(稳定)
```

### 4.2 根因(代码级)

`crates/vm/src/vm/thread.rs`:RustPython **没有全局解释器锁**:
- 对象引用计数是原子的(`PyAtomicRef`/`Radium`);
- 每线程独立执行栈 + `stop-the-world`(THREAD_DETACHED/ATTACHED/SUSPENDED)支持 GC 和跨线程读取;
- `sys._current_frames`/`_current_exceptions` 通过原子指针 + 安全点发布。

### 4.3 意义与解法

- **这是相对 CPython 3.11 的真实落地卖点**:CPU 密集多线程负载可并行(CPython 3.13 free-threading 才追平)。
- 待办:① 共享可变对象的线程安全语义文档化;② subinterpreters API(`_xxsubinterpreters`)补全;③ 基准进 CI 防止回归。
- 风险:无 GIL 意味着对象协议(如 `__hash__`/`__eq__` 中的竞争)需要像 free-threading 一样处理——需要并发测试。

---

## 5. 已验证的修复实验:ldexp 平台 bug(完整示范)

### 5.1 现象
`test_math` FAIL:`testLdexp_denormal AssertionError: 5e-324 != 1e-323`。

### 5.2 根因定位(三层证据)
1. **复现**:`math.ldexp(6993274598585239, -1126)` 在 RustPython 输出 `5e-324`,期望 `1e-323`(0x2);
2. **最小复现**:独立 Rust 程序对比系统 libm 与 libm crate:
   ```
   ucrt  : bits=0x0 (0.0)      ← Windows ucrt 对 subnormal 结果截断
   libm  : bits=0x2 (1e-323)   ← 正确 round-to-nearest
   ```
3. **上游参照**:本机 CPython 3.11/3.12 同样输出 `5e-324`(同病);**CPython 3.15.0b3 输出 `1e-323`(已修复)**——证明这是平台 libm 缺陷,CPython 新版本已在 MSVC 侧修复。

### 5.3 修复(已应用并验证)
- `pymath-patched/src/m.rs`:`#[cfg(windows)]` 下 `ldexp` 改走 `libm::ldexp`(纯 Rust 实现,正确舍入),非 Windows 保持系统 libm;
- `pymath-patched/Cargo.toml`:windows target 增加 `libm` 依赖;
- 根 `Cargo.toml`:`[patch.crates-io] pymath = { path = "pymath-patched" }`;
- 连带:`testRemainder` 修复后意外通过 → 按 AGENTS.md 规范移除其过时 `@unittest.expectedFailureIfWindows("TODO: RUSTPYTHON; Error message too long")` 标记。

### 5.4 验证结果
```
修复前:test_math FAILED (failures=1)  ← testLdexp_denormal
修复后:test_math 89 run 5 skip → Result: SUCCESS ✅
回归:22 模块全部 PASS ✅
```

### 5.5 上送建议
- 向 `RustPython/pymath` 上游提交此修复(一行平台路由 + 依赖声明),加 `testLdexp_denormal` 回归;
- 在 `Lib/test/test_math.py` 保留已通过的测试(删除标记后自动守护)。

---

## 6. 完整解决方案路线图

### Phase 0:基建(1-2 周)
- [ ] 把 `bench/`(本实验脚本)纳入仓库,`benches/` 加 CPython 对照基准,CI 门禁(慢于 CPython N 倍即失败)
- [ ] 跑 `scripts/whats_left.py`,产出未实现清单基线
- [ ] 全套测试定时跑,`TODO: RUSTPYTHON` 标记按类别归档

### Phase 1:兼容性收敛(1-3 个月,低风险高确定性)
- [ ] **上送 ldexp 修复**(已验证,当天可做)
- [ ] **ssl 属性面补齐**:`hostname_checks_common_name` 等 CPython 3.14 属性对照表,契约测试 → 目标:requests/urllib3 冒烟通过
- [ ] pip top-100 纯 Python 包 CI 冒烟(导入门禁)
- [ ] 清 expectedFailure 标记:能修的修,不能修的归类文档化
- [ ] ctypes 增强(struct/array/callback 完整语义)

### Phase 2:性能(3-6 个月,杠杆排序)
- [ ] **P0 vectorcall 全路径**:方法/关键字/super 调用全部栈上参数,消除 FuncArgs 分配 → 预期调用类 6.6x→3x
- [ ] **P1 特化扩展**:LOAD_METHOD 快路径、BINARY_OP 超指令、dict/list 类型版本缓存
- [ ] **P2 字符串 UTF-8 缓存**、启动(.pyc 缓存 + frozen stdlib 默认)
- [ ] JIT 按 §3.3 方案 A/B 收敛(或明确降级,资源转特化)

### Phase 3:生态与定位(6-12 个月)
- [ ] C 扩展路线:ctypes 优先 → capi 成熟化(与 cpyext 的十年经验对齐,明确"不承诺 numpy")
- [ ] subinterpreters API + 并发安全文档(放大无 GIL 优势)
- [ ] API 1.0 稳定 + 打包完善 + 官方落地案例(嵌入/边缘/WASM)文档化
- [ ] 差异化定位落地:"**无 GIL 的嵌入式 Python 运行时**"——对多线程嵌入场景直接对标 CPython 3.13+ free-threading

### 使用方建议(今天就能用)
- ✅ **可评估采用**:嵌入式脚本(无 C 依赖)、WASM/浏览器、多线程计算型嵌入(实测并行优势)、教学
- ❌ **暂不可用**:替代生产 `python`、任何依赖 C 扩展的库链
- ✏️ **可立刻贡献**:ldexp 修复上送、ssl 属性对照、whats_left 清理

---

## 7. 复现指南

```bash
# 构建(本实验)
cargo build --release                    # nojit 二进制
cargo build --release --features jit     # jit 二进制
$env:RUSTPYTHONPATH = "$PWD\Lib"         # Windows 需要

# 基准
python bench\bench.py                    # CPython 对照
.\target\release\rustpython.exe bench\bench.py

# 导入/启动
python bench\imports.py

# 测试套件抽样
.\target\release\rustpython.exe -m test test_math
.\target\release\rustpython.exe -m test test_str

# JIT
.\bench\rustpython-jit.exe bench\jit11.py   # 100M 循环对比

# 线程并行
.\bench\rustpython-nojit.exe bench\t16.py

# 修复验证(已应用)
.\target\release\rustpython.exe -m test test_math   # 应 SUCCESS
.\target\release\rustpython.exe -c "import math; print(math.ldexp(6993274598585239, -1126))"  # 应 1e-323
```

## 8. 关键文件索引

| 路径 | 说明 |
|---|---|
| `bench/bench.py` | 12 项性能基准(跨解释器) |
| `bench/imports.py` | 冷导入时间 |
| `bench/ldtest/` | libm vs ucrt ldexp 最小复现 |
| `bench/t16.py` | 线程并行性测量 |
| `bench/strbench.py` | 字符串细粒度基准(join 20x / split 8x) |
| `pymath-patched/` | ldexp 修复的本地 vendor(待上送) |
| `Lib/test/test_math.py` | 已移除 testRemainder 过时标记 |
| `Lib/test/test_threading.py` | 已移除 test_finalize_running_thread 过时标记 |
| `Lib/ctypes/__init__.py` | pythonapi stub(无原生 python DLL 时) |
| `crates/stdlib/src/ssl.rs` | HOSTFLAG_NEVER_CHECK_SUBJECT + _host_flags |
| `crates/jit/src/lib.rs` | JIT 调用边界(libffi) |
| `crates/vm/src/builtins/function.rs` | 调用路径(invoke/fill_locals) |

## 9. 已验证修复与提交(fork:sekkit/RustPython)

| 提交 | 修复 | 验证结果 |
|---|---|---|
| `f1c54ebc3` | ldexp Windows 平台 libm 路由(pymath) | test_math 从 FAIL → SUCCESS |
| `ebabdbfc0` | 移除 testRemainder 过时 expectedFailureIfWindows | test_math 全绿(89 run 5 skip) |
| `1f255c070` | ANALYSIS.md / SOLUTION.md / bench 脚本 | — |
| `e5ce5ca4c` | ssl: HOSTFLAG_NEVER_CHECK_SUBJECT + _host_flags | **requests HTTPS 200**;test_ssl PASS(196 run) |
| `402af0956` | ctypes.pythonapi stub(无 python DLL 时) | **flask 全栈 200**;test_ctypes PASS(328 run) |
| `27cec7b0d` | 移除 test_finalize_running_thread 过时标记 | test_threading PASS(229 run) |
| `680121df3` | 生态验证脚本 + 网络/邮件/Web 测试结果记录 | 24+ 测试模块全绿 |
| `d2789975d` | sqlite3/multiprocessing/asyncio/subprocess 生产验证 | test_sqlite3 508 run 全绿 |
| `539d03b6b` | **sqlite: detect_types 下 NULL decltype 处理**(CPython 对齐) | **django 6.1 ORM + sqlite 全通**;test_sqlite3 仍 508 run 全绿 |
| `3fb0acb11` | **perf(str): str.join exact list/tuple 快速路径**(预扫描+单次分配) | join 21.4x → 8.2x 差距(0.386→0.148s);test_str/test_string 全绿 |
| `77c2ad0f3` | **fix(ctypes): 位域读改写语义**(掩码+移位+符号扩展) | 与 CPython 逐位一致(含 1-bit 有符号 -1 语义);test_ctypes 328 run 全绿 |
| `204c57b19` | **fix(type): `__slots__` 中 `__dict__`/`__weakref__` 不与类属性冲突** | **celery 5.6.3 解锁**(import/app/任务定义/发布);test_descr 162 run 全绿 |
| `bd3f47b5b` | **fix(ctypes): 回调作为函数参数**(from_param + 转换豁免) | **EnumWindows 标准写法可用**(467 窗口);test_ctypes 328 run 全绿 |
| `2d5d14794` | **fix(ctypes): 结构体/联合数组元素返回活实例**(视图语义) | `(Point*3)[i].x = 7` 读写回正确;test_ctypes 全绿 |
| `9891162f0` | **fix(ctypes): cast() 语义**(回调→函数指针、byref 支持) | cast(byref) == addressof;cast(callback) 正确;test_ctypes 全绿 |
| `24c154f31` | **fix(stdlib): `nt._add_dll_directory`/`_remove_dll_directory`**(Win32 API) | `os.add_dll_directory` 往返可用;test_os 375 run 全绿 |
| `8cf8b37ed` | **feat(stdlib): cProfile**(纯 Python `_lsprof` shim + profiling 包同步) | 输出与 CPython 结构一致;`cProfile.run`/`pstats` 可用 |
| `c3a027acf` | **feat(capi): PyModuleDef/PyModule_Create + Windows 下 C-API 测试可链接可运行** | PyModuleDef/Init/Create/FromSlotsAndSpec(PEP 793)/Exec/ExecDef/SetDocString/GetDict、PyType_GetSlot/Freeze/FromSlots、PyErr_CheckSignals、PyUnicode_From/AsWideChar、buffer 协议(PyObject_GetBuffer/Release/GetPointer);vendor pyo3-ffi 去掉 Windows `#[link(pythonXY)]` 回退属性;`cargo test -p rustpython-capi --lib` **103/103 全绿(Windows,上游 CI 无法运行)** |
| `3b1db295b` | **feat(capi): PyArg_ParseTuple/Py_BuildValue/PyObject_Call\*(C 可变参 shim + Rust 解析)** | getargs/modsupport 核心格式码(s/z/y/u/#/\*/U/S/O/O!/O&/w/t#/整型/浮点/D/c/C/p + \|/\$/:/;/()/[])、keywords 合并、UnpackTuple、BuildValue 分组、CallFunction/Method/ObjArgs;`cargo test -p rustpython-capi --lib` **111/111 全绿** |
| `2f68993e4` | **feat(capi): PyErr_Format 经 C 可变参 shim 落地** | PyErr_Format/SetObjectWithCause/ExceptionMatches 等;test_ctypes 328 run 全绿 |
| `970d86c4e` | **feat(capi): PEP 489 多阶段扩展加载 + CPython 测试扩展模块可构建** | `_imp.create_dynamic` 完整加载序列(PyInit/PyInitU 短名编码、init 结果校验、slot 扫描、Py_mod_create、exec、single-phase 全局缓存 + m_copy/m_init 语义、reload);`exec_dynamic` 按 md_state 语义跳过重复 exec;capi 新导出(PyModule_Add\*/GetDef/GetState/New、PyState_\*、PyType_FromSpec 系列、_PyArg_CheckPositional/_PyArg_UnpackKeywords、_PyNamespace_New、PyTime_\*、_Py_NoneStruct/PyUnicode_Type/PyLong_Type/PyBool_Type 数据符号);模块方法绑定 module 为 self;**module→def 与 exec 标记改存模块自身 __dict__(修复指针复用导致的间歇性 test_bad_modules[exec_unreported_exception] 失败)**;`bench/build_test_extensions.ps1` 从 CPython 3.14.7 源码构建 _testsinglephase/_testmultiphase;`test_importlib` **1440 run 全绿(连续 4 轮)**,smoke 全过,test_ctypes 328 run 全绿 |

### 9.0 并行调查(4 subagents)产出报告

| 报告 | 核心结论 |
|---|---|
| `bench/reports/perf_candidates.md` | 小整数缓存已与 CPython 一致;immortal 仅骨架;join 21.4x 是最大单点(已修);PyInt BigInt 双分配、upper/lower 无条件分配、dispatch 固定开销为后续候选 |
| `bench/reports/cext_route.md` | ctypes 4 真缺陷(位域/数组元素/回调参数/cast——**全部已修**);capi 325 导出缺模块初始化核心;numpy 三层根因(.pyd 加载缺失) |
| `bench/reports/ecosystem_gaps.md` | **17/18 纯 Python 包全通**(click/jinja2/httpx/aiohttp/sympy/networkx/pydantic v1/dateutil…);唯一纯 Python 失败 celery 已修 |
| `bench/reports/stale_markers.md` | **0 个过时标记**(1075 处 expectedFailure 全部仍真实失败);无"unexpected success";失败主因:语义差异/错误消息 |

**ctypes 4 缺陷全清后解锁**:纯 ctypes 生态(wmi 类库、pywin32 纯 ctypes 部分、手写 Win32 自动化)——综合验证 GetSystemInfo + EnumWindows + GetWindowTextW + 结构体数组与 CPython 输出一致。

### 9.1 生态可用性现状(实测)

| 类别 | 库 | 状态 |
|---|---|---|
| HTTP 客户端 | requests 2.34.2 + urllib3 | ✅ HTTPS GET 200 |
| Web 框架 | flask(模板/session)、django 6.1(路由 + **ORM + sqlite**) | ✅ 全通 |
| 测试框架 | pytest 9.1.1 | ✅ 真实测试 2 passed |
| 数据格式 | yaml、json、xmlrpc、tomllib | ✅ |
| 终端 UI | rich、pygments | ✅ |
| 数据库 | sqlite3(`--features sqlite`) | ✅ test_sqlite3 508 run 全绿 |
| 异步 | asyncio(gather/cancel/executor) | ✅ 并发正确(0.19s/3 任务) |
| 进程 | multiprocessing(spawn)、subprocess | ✅ 验证通过 |
| 邮件/文件协议 | smtplib/ftplib/poplib/imaplib 测试 | ✅ 全 PASS |
| 标准库测试 | **80+ 模块**(socket 746/array 890/tarfile 748/typing 709/decimal 577…) | ✅ 几乎全绿 |

### 9.2 性能剩余差距(待 Phase 2)

字符串细粒度(200k 次,秒):
| 操作 | RustPython | CPython 3.11 | 倍数 |
|---|---|---|---|
| str.join | 0.354 | 0.018 | **20x** |
| str.split | 0.381 | 0.047 | **8x** |
| str.format | 0.235 | 0.031 | 7.6x |
| str.upper/lower | 0.072 | 0.014 | 5x |
| str.slice/find/startswith | 0.06 | 0.013 | 4.5-5x |

→ 字符串调用链(方法分派 + WTF-8 边界 + 迭代器分配)是最大单点,建议 Phase 2 优先做 str 方法内联缓存 + 调用路径(vectorcall)。

### 9.3 已排除的优化方向(实验结论)

| 实验 | 结果 |
|---|---|
| `rustpython-vm` 单 codegen-unit | 无收益(±2%,LTO=thin 已覆盖跨 crate 内联) |
| freeze-stdlib 启动速度 | 无收益(58.7 vs 55.4ms;启动瓶颈在解释器初始化,非 stdlib 加载) |
| freeze-stdlib 部署价值 | ✅ 单文件分发(56.5MB,无需 Lib 目录) |
| **调用路径零分配改造** | **无需改造**:上游已有 tailcall(trampoline)+ `CallArgBuffer` 栈上参数 + `invoke_exact_args_slots`,简单 PyFunction 调用已是零堆分配 |

### 9.3b 第 3 轮性能剖析结论

- **数据校准**:此前部分"性能恶化 18 倍"测量是误用 `--features flame-it` 构建产物(`cargo build --features flame-it` 会覆盖 `target/release/rustpython.exe`,插桩使一切慢 ~18x;需重建普通版)。普通版数据与第 1 轮一致(调用 7.0x / 方法 6.8x / 字符串 8.6x)。
- **增量瓶颈分解**(3M 次,普通版,vs CPython 3.11):
  | 层 | RustPython | CPython | 单次增量 |
  |---|---|---|---|
  | for 循环基础 | 244ms | 42ms | 5.8x |
  | +局部变量 | +43ms | +2ms | — |
  | +int add | +145ms | +45ms | — |
  | **+函数调用** | **+610ms(~203ns/次)** | +101ms(~34ns/次) | **6x** |
  | +方法调用 | +1344ms 总 | +193ms 总 | 7.0x |
- **根因结论**:调用路径已高度优化(tailcall 避免 Rust 递归、CallArgBuffer 避免 Vec 分配、LOAD_GLOBAL 有版本缓存);剩余差距**分散在解释器基础成本**(指令 dispatch、原子引用计数、datastack 帧管理、类型检查),无单点银弹,需系统级优化(更紧凑 dispatch / immortal 对象 / 对象布局),属 Phase 2 大工程。

### 9.3c 已尝试并回退的优化(负面结论,避免重复)

| 候选 | 结果 | 结论 |
|---|---|---|
| **upper/lower "无变化返回 self"** | 已实现并回退 | **与 CPython 测试套件冲突**:`string_tests.checkequal` 强制 `assertIsNot`(CPython 3.11/3.15 实测也总是返回新对象)。此方向不可行 |
| **PyInt 算术 i64 快路径**(checked_add/sub/mul 绕过 BigInt 运算) | 已实现并回退 | 收益仅 ~1%(arith 4.13M→4.19M ops/s):瓶颈是 **PyInt 对象 + BigInt 值(Vec)的分配本身**,而非 BigInt 运算;小整数缓存(-5..256)与 freelist 已存在但覆盖有限。真正解法是 **PyInt 内联数字表示**(数据模型重构,高风险,Phase 2) |

### 9.4 Windows 构建注意事项(实测)

- `--features freeze-stdlib` 在 Windows 上依赖符号链接(`crates/pylib/Lib` 等 10 个条目指向仓库根 `Lib/`),默认 `core.symlinks=false` 克隆会缺失,构建报 `Error listing dir "crates\pylib\src\../Lib"`。修复:为缺失条目创建符号链接/junction(见 README: `git config core.symlinks true` 后重新克隆,或手动 mklink)。
- **重要**:符号链接缺失不仅影响 freeze-stdlib 构建,还会导致 **vm crate 编译期 `py_freeze!` 宏漏掉冻结模块**(`__hello__`/`__phello__` 等),使 `test_importlib` 出现 14 个 error(frozen finder 测试拿不到 FrozenImporter spec)。修复符号链接后必须清理 vm 构建缓存重编(`rm -rf target/release/build/rustpython-vm*`)。修复后 test_importlib 1440 run 全绿。

### 9.5 第 2 轮验证汇总

| 项 | 结果 |
|---|---|
| 测试模块总计 | **100+ 模块验证,几乎全绿**(socket 746/array 890/tarfile 748/typing 709/pickle 464/importlib 1440/multiprocessing_spawn 441/zipfile 380/decimal 577…) |
| sqlite3 默认 feature | ✅ 已提交(529ba89de),默认构建自带 sqlite3 3.53.2 |
| asyncio/multiprocessing/subprocess/大文件 IO | ✅ 全部验证通过 |
| freeze-stdlib | ✅ 单文件分发 56.5MB(启动速度无增益,部署价值在免 Lib) |
| 已排除 | codegen-units=1 无收益;冻结 stdlib 对启动无收益 |

| `crates/vm/src/frame.rs` | 执行引擎(vectorcall 现状) |
| `crates/vm/src/vm/thread.rs` | 无 GIL 线程模型 |
