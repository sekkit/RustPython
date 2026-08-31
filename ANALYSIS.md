# RustPython 代码分析

> 生成日期:2026-08(基于 `main` @ `70b47dd7c`)
> 注:本文档为本地分析笔记,非上游项目文档。

## 第一部分:架构分析

### 1. 项目概览

RustPython 是**用 Rust 实现的完整 Python 3 解释器**(目标兼容 CPython 3.14+),不是 CPython 绑定。定位:可嵌入 Rust 应用的 Python 环境 + 可编译到 WASM 在浏览器运行 + 库形式发布(docs.rs 可用)。

- **规模**:约 32 万行 Rust(vm 13 万 + codegen 5.4 万 + stdlib 4.6 万 + doc 3.2 万 + 其余),外加 `Lib/` 下 **1729 个直接从 CPython 复制的 .py 文件**(纯 Python 标准库 + 完整 CPython 测试套件)
- **成熟度**:594 个贡献者,版本 0.5.0,Rust edition 2024 / rustc 1.95,提交持续到今天(每天多个 PR),有 NSIS/WiX 安装器配置和 venvlauncher
- **对照参照**:与 RustScript(自创脚本语言,~8 万行)完全相反的工程路线——"重新实现一门成熟语言"

### 2. Workspace 结构:20 个 crate 的精细分层

```
源码 → [ruff 解析器(外部)] → AST → codegen(符号表+编译) → CPython 兼容字节码 → VM 执行帧
                                    ↘ compiler-core(字节码定义+marshal,no_std)
                                    ↘ jit(Cranelift,实验性)
对象层:vm/object(Py<T>/PyObject/GC/QSBR) ← derive(#[pyclass] 宏)
标准库三分布局:vm/stdlib(Rust,VM 必需) + stdlib(Rust,面向用户) + Lib/(Python,从 CPython 复制)
外延:capi(C API 兼容层) + wasm + pylib(冻结 stdlib) + venvlauncher
```

| Crate | 行数 | 职责 |
|---|---|---|
| `vm` | 131,681 | 对象模型、内建类型、帧执行、GC、VM 核心 |
| `codegen` | 54,290 | AST→字节码编译器(对照 CPython compile.c) |
| `stdlib` | 46,254 | Rust 实现的用户模块(math/json/socket/ssl/sqlite3/tkinter…) |
| `doc` | 31,602 | 从 CPython 抓取的权威 docstring |
| `compiler-core` | 6,726 | 字节码定义、marshal、varint(可 no_std) |
| `compiler` | 5,221 | 解析器与 codegen 之间的粘合 |
| `capi` | 6,515 | CPython C API 兼容层(能加载真 C 扩展) |
| `jit` | 2,997 | Cranelift JIT(自述 "very experimental") |
| `sre_engine` | 2,209 | CPython sre 正则引擎移植 |
| `wtf8` | 1,878 | WTF-8 字符串(内部字符串表示,对应 CPython PEP 393) |
| 其余 | — | common/derive/derive-impl/literal/unicode/pylib/wasm/host_env/venvlauncher |

### 3. 核心架构决策(六个关键点)

#### 3.1 解析器不自研——复用 Ruff

parser 直接用 `ruff_python_parser`(RustPython 自己 fork 的 Ruff,tag `0.15.19-rustpython`)。Python 语法复杂度由 Ruff 团队维护,RustPython 只做语义和运行时。这是"重新实现成熟语言时前端要复用"策略的典型印证。

#### 3.2 字节码:完整复刻 CPython 3.11+ 指令集

`compiler-core/src/bytecode/` 定义了 96+ 个指令的 `Instruction` 枚举,紧跟 CPython 现代设计:

- **异常表(zero-cost exception handling)**:side table 记录 `(start, end, target, depth, push_lasti)`,varint 编码——与 CPython 3.11+ 完全同构
- **自适应特化(adaptive specialization)**:`specialize_load_attr / specialize_binary_op / specialize_call / specialize_send / specialize_compare_op / specialize_to_bool…`——复刻 CPython 3.11 的 specializing interpreter:首次执行慢路径、回填内联缓存、下次直接走特化快路径(如 `BinaryOpInplaceAddUnicode` 超指令带目标局部变量缓存)
- **marshal 格式**兼容(.pyc 生态)

#### 3.3 对象模型:手工 vtable 的 PyObject

`object/core.rs`:`PyObjectRef` 不是 `PyRc<dyn Payload>`(双指针宽 fat pointer),而是**单指针 + 手工 vtable**(`PyObjVTable` 存类型 ID 和 drop/traverse 函数)。payload 访问通过泛型下转:先比 TypeId,再直接指针 cast 到 `*const PyInner<T>`,避免 trait 对象的 vtable 偏移查询。类型系统矩阵:`Py<T>`(解释器无关)/`PyRef<T>`(引用计数)/`PyObjectRef`(类型擦除)/weak 引用。

#### 3.4 内存管理:引用计数 + 可选分代 GC + QSBR + trashcan

- **引用计数**为主(`RefCount`,原子操作,多线程安全)
- **`gc` feature(默认开启)**:分代 GC(`GcState`/`GcGeneration`/`collect`),处理循环引用;GC 关闭时泄漏循环(嵌入式可裁剪)
- **QSBR**(quiescent-state-based reclamation):锁自由类型方法缓存的内存回收,"mirrors _Py_qsbr"
- **Trashcan**:递归 deallocation 深度限制(50),超限入队延后处理,防止深层嵌套结构释放时栈溢出——CPython `Py_TRASHCAN` 的等价物
- 对象 traverse 机制(`Traverse`/`TraverseFn` derive)供 GC 精确扫描

#### 3.5 帧执行:trampoline + 数据栈 + 尾调用

`frame.rs`(**11,540 行/503KB,全仓库最大文件**)是执行引擎核心:

- `ExecutionResult = Return | Yield | TailCall`——支持**尾调用**:字节码循环把已准备好的下一帧放到 `vm.pending_tailcall_frame`,由 trampoline 接力,避免深递归时 Rust 栈溢出
- **DataStack**:线程本地 bump-allocate 栈,非生成器帧的 `InterpreterFrame + localsplus` 直接在上面连续分配,零堆分配
- `FrameObject`(Python 可见的 frame 对象)与 `InterpreterFrame`(执行态)分离,用 `UnsafeCell` + 原子 owner 字段管理,堆分配后手工修补裸指针
- 生成器/协程/异步生成器:帧可挂起(`resume`/`gen_throw`/`yield_from_target`)

#### 3.6 标准库三分布局(兼容性策略的精髓)

| 层 | 位置 | 内容 |
|---|---|---|
| VM 必需(Rust) | `vm/src/stdlib/` | sys、os/posix、`_io`(240KB)、`_thread`、marshal、gc、itertools、`_ast`、`_ctypes`、`_winapi` |
| 用户模块(Rust) | `crates/stdlib/` | math、json、re、socket、ssl(rustls/openssl 双后端)、sqlite3、zlib/bz2/lzma、csv、random、unicodedata(ICU)、tkinter |
| 纯 Python | `Lib/` | 直接从 CPython 复制,保守修改;`pylib` crate 可冻结进二进制(freeze-stdlib) |

配合 `capi`(C API 兼容)使 CPython 生态资产最大化复用;测试也直接用 CPython 的 `Lib/test`。

### 4. 工程实践亮点

1. **严格的 lint 纪律**:workspace 级 clippy 配置启用上百条规则(pedantic/nursery 逐步启用)
2. **性能回归基准**:`benches/`(criterion,execution + microbenchmarks);`flame-it` feature 火焰图
3. **依赖选型讲究**:bigint 用 malachite、Unicode 用 ICU 2.x、SSL 默认 rustls+aws-lc(FIPS 可选)、字符串内部 WTF-8
4. **`whats_left.py` 脚本**:自动对比出未实现方法清单,用工具管理"兼容性长尾"
5. **AI 协作规范**:AGENTS.md 定义 AI 贡献策略(披露 trailer、测试规则、Lib/ 修改红线)
6. **多目标产物**:CLI(含 REPL)、capi 动态库、wasm(wasm32-wasip1)、NSIS/WiX 安装包、venvlauncher

### 5. 与 RustScript 对照(两条路线的实证)

| 维度 | RustScript | RustPython |
|---|---|---|
| 语言 | 自创(Rust 风格脚本) | 复刻既有语言(Python 3.14) |
| 规模 | ~8 万行,1 人主导,2026 新项目 | ~32 万行,594 贡献者,7 年项目 |
| 解析器 | 自写(语言小,可行) | **复用 Ruff**(语言大,必须复用) |
| 字节码 | 26 opcode 极小内核,高层操作走宿主 | 96+ 指令,完整复刻 CPython(含异常表/内联缓存) |
| 类型系统 | 编译期渐进推断(自有设计) | 无(动态语言,匹配 duck typing 协议) |
| 执行优化 | 解释快路径 + 追踪 JIT + AOT(Cranelift) | 自适应特化解释器(照抄 CPython 3.11 思路)+ 实验性 JIT |
| 内存 | Arc 引用计数(循环即泄漏) | 引用计数 + 分代 GC + QSBR + trashcan |
| 值表示 | `enum Value`(9 变体) | 手工 vtable 的 PyObject(单指针) |
| 标准库 | 自举(RSS 写) | 三分:Rust 核心 + Rust 用户模块 + CPython 复制 |
| 生态兼容 | 无(自有宿主 API) | C API 层 + .pyc marshal + CPython 测试套件 |
| 嵌入方式 | `#[pd_host_function]` 宏 | `#[pymodule]/#[pyclass]` 宏 + capi |

**最有趣的一点**:两者都选了 Cranelift 做 JIT,但优化主战场完全不同——RustScript 押注 JIT/AOT 原生码,RustPython 押注**解释器内部优化**(内联缓存、特化超指令、帧的 bump 分配、尾调用 trampoline),因为它要对标的是 CPython 本身,而 CPython 的性能基线就是特化解释器。

### 6. 架构小结

RustPython 是"重新实现一门成熟语言"的教科书案例:**能借的全借**(解析器、docstring、Lib、测试套件、指令集设计、GC/trashcan/QSBR 机制全部对齐 CPython),**必须自己写的部分用 Rust 的类型系统做到极致安全边界内的高性能**(手工 vtable、UnsafeCell 帧、datastack)。自研火力全部集中在语义运行时 + 兼容性长尾上。

---

## 第二部分:落地应用现存问题

> 项目自述(README):"RustPython is not totally production-ready"(可在 WASM、嵌入 Rust 项目等场景尝鲜)。

### 1. 性能:仍显著慢于 CPython(且差距在被拉大)

- **解释器本体**:普遍比 CPython 慢数倍。CPython 3.11+ 引入自适应特化解释器 + 内联缓存 + 零成本异常,3.13/3.14 又加了 JIT——RustPython 在"追赶复刻"这套设计(代码里有 `specialize_*` 特化路径和异常表),但没追平,社区仍在持续做解释器优化(指令分派、栈帧与内存布局等,2025-12 仍有相关优化文章)。
- **具体性能坑有据可查**:
  - 排序比 CPython 慢(Issue #6093: Replace rust-timsort so RustPython sorting speed ~= CPython);
  - vectorcall 调用约定未完整落地(Issue #7362: vectorcall per type),函数调用开销高——恰是 Python 性能命门。
- **JIT 是 "very experimental"**(README 原话):默认不启用(`--features jit`),按函数手动 `foo.__jit__()` 触发,构建还需 autoconf/automake/libtool/clang。与 CPython 3.13 copy-and-patch JIT、PyPy tracing JIT 不在一个可用性层级。
- **内存管理开销**:引用计数 + 分代 GC + QSBR + trashcan 全套复刻,原子引用计数多线程下有争用;GC 是可选 feature,关掉就泄漏循环引用。

### 2. 兼容性:与 CPython 的"最后 10%"极长

本地统计 `Lib/test/`(CPython 原版测试套件)标记:

| 指标 | 数值 |
|---|---|
| `TODO: RUSTPYTHON` 标记 | **1292 处** |
| 含标记的测试文件 | **185 / 437 个**(42% 的测试文件含 skip/expectedFailure) |

- **C 扩展生态基本不可用**:capi 兼容层 + ctypes(libffi)只覆盖一小部分;numpy/pandas/lxml/pydantic-core 等依赖原生扩展的包跑不了。社区应对是重造轮子(如 rumpy: numpy reimplementation for rustpython),侧面说明原版不可用。**数据科学、ML、Web 后端(Django/Flask 全家桶)全部出局。**
- **版本追逐跑步机**:目标锁定 CPython 3.14,但 CPython 每年演进(3.13 free-threading、3.14 subinterpreters 转正),RustPython 永远在追赶;语言特性也有缺口(测试因缺 PEP 695 支持挂标记)。
- **字节码非稳定接口**(架构文档原话),marshal 兼容但无分层保证。

### 3. 并发模型:有 GIL、没有 free-threading、没有 subinterpreters

- `_thread`/`threading` 是真 OS 线程 + GIL 等价物,同 CPython 传统模型;
- **PEP 703(free-threading)无、PEP 734/684(subinterpreters)无**——在多核扩展性上反而落后于它模仿的对象;
- 唯一多核出路是**多 VM 实例**(每个 `VirtualMachine` 完全隔离,适合"每请求一个解释器"的嵌入场景),但内存成本高且跨 VM 无共享。

### 4. 工程与可维护性风险(选型时要评估)

- **unsafe 重灾区**:手工 vtable、UnsafeCell 帧 + 裸指针修补(`init_iframe_ptrs`)、QSBR、trashcan、trampoline——性能必需,但正确性审计负担大;
- **巨型文件**:`frame.rs` 11,540 行/503KB、`vm/mod.rs` 130KB、`codegen/compile.rs` 1.6MB,review 和入门成本高;
- **双 fork 同步负担**:`Lib/` 是 CPython 标准库的 fork(手动同步);解析器是 Ruff 的 fork(定期跟上游 rebase);docstring 靠 rustpython-doc 从 CPython 拉取——三处持续维护成本;
- **构建重**:20 crate workspace,dev profile 就要给依赖开 `opt-level = 3` 才能用(Windows debug 模式会栈溢出,README 明示要 `--release`)。

### 5. 分发与打包摩擦

- pip 只在启用 ssl feature 后可用(`--install-pip` 手动步骤);Windows 要么装 OpenSSL、要么 `ssl-openssl-vendor`(还需 C 编译器 + perl + make);
- README 自述 "doesn't provide a well-packaged installation",venv 是官方推荐的规避手段;conda 版本非官方;
- Windows 需设 `RUSTPYTHONPATH` 指向 `Lib`;
- 嵌入侧 API 是 0.5.0(pre-1.0),semver 允许破坏性变更,长期跟随升级有成本。

### 6. 现实落地图谱

**已被验证的场景**:
- ✅ WASM/浏览器(官方在线 demo,最成熟的脸面)
- ✅ 嵌入 Rust 应用的受控脚本:GreptimeDB(嵌入式脚本)、tauri-plugin-python-api(Tauri 桌面应用,实验性)、游戏逻辑脚本(pyckitup、Robot Rumble)
- ✅ 教学/实验/REPL

**不适合的场景**:
- ❌ 替代系统 `python` 跑任意生产代码(性能 + 兼容双杀)
- ❌ 依赖 C 扩展生态的任何工作(数据科学、爬虫全家桶、密码学库链)
- ❌ 高并发多核服务(free-threading 缺位,单 VM 内并行无解)
- ❌ 需要稳定嵌入 API 长期不改的封闭产品(0.x 版本)

### 7. 结论:占位很好,兑现未满

三个初衷——纯 Rust 无 CPython 绑定、干净实现、WASM 可编译——**都已实现**;卡住落地的是三件事:**性能追不上被它模仿的 CPython 3.11+**、**C 扩展生态无法复刻(numpy 们绕不过去)**、**并发模型停留在 GIL 时代**。这三件恰是"能替换 CPython"的必要条件。

目前的真实生态位:**"Rust 应用里嵌一个隔离的、内存安全的 Python 子集脚本引擎"和"浏览器里的 Python"**,而不是通用 Python 运行时。

落地建议:嵌入脚本场景(GreptimeDB 模式)今天可以评估采用,但要接受兼容子集和 0.x API;任何"跑现有 Python 代码"的想法,现阶段不如直接用 CPython。

---

## 附:关键文件索引

| 文件 | 说明 |
|---|---|
| `architecture/architecture.md` | 官方架构文档(组件图) |
| `crates/vm/src/frame.rs` | 帧执行引擎(11.5K 行,字节码循环/特化/trampoline) |
| `crates/vm/src/vm/mod.rs` | VirtualMachine 核心 |
| `crates/vm/src/object/core.rs` | PyObject/Py<T> 对象模型与手工 vtable |
| `crates/vm/src/gc_state.rs` | 分代 GC |
| `crates/vm/src/object/qsbr.rs` | QSBR 无锁内存回收 |
| `crates/compiler-core/src/bytecode/` | 指令集定义(96+ 指令、异常表、oparg) |
| `crates/codegen/src/compile.rs` | AST→字节码编译器(对照 CPython compile.c) |
| `crates/codegen/src/symboltable.rs` | 符号表/作用域分析 |
| `crates/jit/src/lib.rs` | Cranelift JIT(实验性) |
| `crates/derive/` + `crates/derive-impl/` | #[pyclass]/#[pymodule] 宏实现 |
| `crates/stdlib/` + `crates/vm/src/stdlib/` | Rust 侧标准库(两层) |
| `Lib/` | CPython 复制的纯 Python 标准库 + 测试套件(1729 个 .py) |
| `scripts/whats_left.py` | 未实现清单生成器 |
| `AGENTS.md` | AI 协作规范(测试修改红线、Lib/ 保守修改) |
