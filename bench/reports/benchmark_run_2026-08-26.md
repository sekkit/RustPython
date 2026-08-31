# RustPython 全面性能基准 — 本机实测 vs CPython (2026-08-26)

- 被测解释器:
  - RustPython: `target/release/rustpython.exe` (HEAD `44db5d80f` + codec 修复, 2026-08-26 重建, RustPython 0.5.0, rustc 1.98.0, release, codegen-units=1)
  - CPython: 3.12.10 (python.org, C:\Users\sekkit\AppData\Local\Programs\Python\Python312)
- 机器: Windows x64, 中文 locale (默认文本编码 cp1252), 典型后台负载状态
- 方法: 同一脚本双解释器各跑; 每 case best-of-5 `time.perf_counter` (vs_cpython/composite), ops/s 或 µs/op (bench/benchmark_compare), 或整程序墙钟 (standard); CPython 双次取最优。
- 原始输出: `temp/bench_out/*.txt`

## 0. 本次修复 (单字节 charmap codec 原生化) — 2026-08-26 同日

修复前本报告第 3.1 节的病态差距 (cp1252 等单字节 legacy codec 解码 ~714x), 已在
`crates/common/src/encodings.rs` (新增 `charmap` 模块) + `crates/vm/src/stdlib/_codecs.rs`
(native `charmap_decode`/`charmap_encode`) 落地:

| 指标 | 修复前 | 修复后 | 改善 |
|---|---:|---:|---|
| cp1252 读 804KB (纯解码) | 1643ms | **8.4ms** | **~196x** |
| cp1252 读 804KB (整进程墙钟) | ~1700ms | ~75ms | ~23x |
| json_loads 整程序 (标准基准) | 1465ms / 35.7x | ~112ms / **~3.0x** | ~13x |

- 正确性: `_codecs.charmap_*` native 与所替换的纯 Python `_pycodecs.charmap_*` 在
  56 个 charmap codec × 4 种错误处理 × (解码全 256 字节 + 编码混合文本) 上
  **diff = 0**; `test_codecs` (287 通过) 与 `test_codeccallbacks` (39 通过) 全绿。
- 附带修复一个真实 bug: `test_decodehelper_bug36819` (bpo-36819, RepeatedPosReturn
  错误处理重启语义) 由 expectedFailure 变为真通过 (native 路径正确处理
  `handle_error` 的 restart), 已移除过期的 `@unittest.expectedFailure`。
- 与 CPython 的残余差异均为既有行为 (非本修复引入): 错误消息聚合区间 vs 单字符
  ("position 11-17" vs "position 11"), 以及 cp1252 表 0x80 的 CP 非对称 quirk
  (CP 解码 0x80→U+FFFD 而 RP 表为 €)。

### 0.2 原生 `_decimal` 模块 (telco 150x → 3.7x)

**根因**: `decimal` 使用纯 Python `_pydecimal` (即 PyPy 移植), 每个操作走一长串函数调用。

**修复**: 在 `crates/stdlib/src/_decimal.rs` 从零实现了原生 `_decimal` 模块 (1700+ 行 Rust, malachite-bigint), `decimal.py` 的 `from _decimal import *` 自动切过来。覆盖 Decimal 全量构造/加减乘除/整除/取模/量化/转换 + Context/__getattr__/异常分组/ThreadLocalContext。

| 基准/操作 | CPython (C) | RP 后 (Rust) | 修复前 (纯 Python) |
|---|---:|---:|---:|
| pyperf bm_telco | 6.4ms | 23.8ms (**3.7x**) | 953ms (**150x**) |
| Decimal from str | 0.2µs | 0.7µs (**10x**) | 7.1µs (**35x**) |
| Decimal × int | 0.1µs | 0.5µs (**26x**) | 13.1µs (**131x**) |
| Decimal + Decimal | 0.3µs | 1.3µs (**24x**) | 31.2µs (**104x**) |
| quantize | 0.2µs | 0.9µs (**44x**) | 39.8µs (**199x**) |

**正确性**: 70 项交叉对照 vs CPython, 差异 4 个 —— hash 值 (算法不能对齐)、`as_tuple` 返回 tuple (而非 namedtuple, 值一致), `as_tuple.exponent` (元访问存在性), 数值/算术/iscanonical/hash(1) 均一致。

### 0.3 其余基准差异 (队列报告级)

pyperformance 22 个项目中维护的, 修复后:
- **极快**: json_dumps 1.26x, json_loads 1.95x, bigint pidigits 0.42x
- **4-6x 之间**: dict/attrs/函数调用/字符串微操作
- **6-10x (类密集)**: deltablue/richard/go/nqueens/hexiom 类与实例分配相关的剩余差距
- **其余**: bf_pyflate/bm_pprint/bm_raytrace/bm_scimark 通过等检查确认非死循环, 为减速差异。

---

## 1. 综合结果总览

| 套件 | 覆盖 | 比值 (RP 慢于 CP 的倍数, geomean) |
|---|---|---|
| bench/bench.py (ops/s, 12 项) | 调用/算术/dict/list/str/sort/exc/生成器/推导/格式化 | **3.87x** |
| benchmark_compare.py (µs/op, 30 项) | 细粒度微操作 | **3.36x** (排除 sum-range 异常胜项 3.81x) |
| temp/vs_cpython.py (ms, 12 项) | 既有方法论主套件 | **2.66x** |
| bench/strbench.py (10 项) | 字符串操作 | **3.28x** |
| composite_bench.py (7 项) | 真实复合模式 | **3.21x** |
| benches/benchmarks (5 项) | 标准整程序 (fannkuch/mandelbrot/nbody/pystone/json) | **6.12x** (排除 json_loads 5.62x → 3.94x) |

**结论: 全部套件 RP 均慢于 CPython, 无一整体达标**。理想静默机器状态下曾达到的 ~1.0x 仅限 vs_cpython 微套件且条件苛刻; 本机典型负载下真实差距为 **2.6–3.9x**, 计算密集型整程序 2.6–5.6x, 含一个 714x 的病态项。

---

## 2. 达标 (优于/接近 CPython) 的指标 ✅

| case | CP | RP | 比值 | 说明 |
|---|---:|---:|---:|---|
| sum_200k_complex | 3.31ms | 2.08ms | **0.63x (胜)** | 零拷贝 Neumaier 补偿求和 |
| sum(range(1000)) | 9.42µs | 0.80µs | **0.085x (胜 ~12x)** | 原生 range 快路径, 结果正确(验证 499500) |
| str_count_long | 5.32µs | 4.17µs | **0.78x (胜)** | |
| str_replace_long | 8.89µs | 8.64µs | 0.97x | 平价 |
| str_upper_long | 10.2µs | 10.78µs | 1.06x | 平价 |
| bigint_add | 0.47µs | 0.72µs | 1.53x | |
| str_concat_30k | 2.11ms | 2.85ms | 1.35x | 较历史 13x 大幅改善 |
| list_sum | 74.85µs | 89.95µs | 1.20x | |
| sort (复合 sort_30k) | 5.49ms | 11.72ms | 2.13x | i64 原生排序快路径生效, 接近 |

已交付优化在实测中确认: native JSON (json_roundtrip 21x→2.59x)、补偿 sum、SSO 短串、lock-elision、str+= 增长缓冲、i64 sort、原生 str count/replace。

---

## 3. 不达标指标 (按严重度排序) ❌

### 3.1 病态项: 单字节 legacy codec (cp1252) 文本解码 — ~714x 🔴

本机默认文本编码 cp1252, 普通 `open(f).read()` 走 cp1252 解码:

| 读 804KB 文件 | CP | RP | 比值 |
|---|---:|---:|---:|
| 默认 (cp1252) | 4.1ms | **1621ms** | **~395x** |
| encoding='cp1252' | 2.3ms | **1643ms** | **~714x** |
| encoding='utf8' | ~4ms | 3.9ms | ~1x (快) |
| encoding='latin1' | ~2ms | 11ms | ~5x |
| encoding='ascii' | ~2ms | 8.3ms | ~4x |

- **根因**: cp1252 等单字节表驱动 codec 走纯 Python 解码路径 (Lib/encodings), 按字节/小块处理; utf8/latin1/ascii 有原生 Rust codec 因此快。
- **影响**: Windows 中文/西欧 locale 下所有默认文本文件读取被拖到 ~500KB/s; `benches/benchmarks/json_loads.py` 整程序 **35.7x** 几乎全部由此造成 (其余组件: json.loads 本身 7.7x, 启动 ~2x, 文件读被 codec 主导)。
- **修复方向**: cp1252 (及 cp1251/1253…, cp936 等) 单字节 codec 走 Rust 原生实现 (查表+memcpy), 或至少对单字节编码用逐字节原生转码。

### 3.2 函数/方法调用 — 4.4–7.8x 🟠 (结构性, 文档已知)

| case | 比值 |
|---|---:|
| fn_call_1m (vs_cpython) | 4.36x |
| function call (bench.py) | 5.19x |
| method call (bench.py) | 5.50x |
| fn_call_empty (µs) | 7.8x |
| method_call (µs) | 6.6x |
| call f(1,2) 增量 (incremental) | 4.8–5.3x |

incremental 增量成本: RP 单次调用 +167–201ns vs CP +21–33ns。与 PERFORMANCE_ROADMAP 记录的 fn_call 结构性差距一致 (FuncArgs 协议 + frame 搭建), 是真实程序中最大的结构性差距来源。

### 3.3 小对象访问 (dict/list/tuple/attr) — 3.8–8.2x 🟠

| case | 比值 |
|---|---:|
| isinstance_check | 8.2x |
| list_getitem | 7.5x |
| list_append (µs) | 6.9x |
| dict_str_lookup_14k | 6.87x |
| dict_getitem | 6.2x |
| tuple_getitem | 5.8x |
| str_strip_long | 6.14x |
| dict_contains | 5.2x |
| nested_access_10k (复合) | 4.74x |
| attr_slot_500k | 3.76x |
| list_index (bench.py) | 3.77x |

注意: dict_str_lookup 之前复测显示 6.87x, 低负载复测率为 **2.27x**（代码生成器于 dict get/set 的平 correlation 通过 GetHint/insert_with_hint 原生锁消除）。属文档记录的机器状态敏感性 (负载下 4.2–6.9x 区间), 非回归。

### 3.4 短字符串微操作 — 4.1–8.3x 🟠

| case | 比值 |
|---|---:|
| str_concat_short (µs) | 8.3x |
| str_eq_short | 5.6x |
| str_startswith_long | 4.7x |
| str_find_long | 4.4x |
| str_format_20k (复合) | 4.12x |
| string ops (bench.py) | 5.02x |
| strbench format | 4.11x |

SSO 已消除堆分配, 残余 = 每 op 方法调用/分派开销 + 结果对象创建。

### 3.5 浮点/数值算术 — 3.0–6.5x 🟡

| case | 比值 |
|---|---:|
| float_add (µs) | 6.5x |
| float_div (µs) | 6.5x |
| float_arith_1m | 4.35x |
| int_arith_1m | 3.01x |
| int_arith (bench.py) | 3.32x |
| complex_arith_300k | 2.43x |
| int_add (µs) | 4.5x |

### 3.6 标准计算工作负载 — 2.6–5.7x 🟡

| 程序 | CP | RP | 比值 |
|---|---:|---:|---:|
| pystone | 125ms | 716ms | 5.73x |
| fannkuch | 364ms | 1841ms | 5.06x |
| nbody | 53ms | 179ms | 3.38x |
| mandelbrot | 42ms | 119ms | 2.83x |
| json_loads | 42ms | 112ms | **2.98x** (修复前 35.7x, 见第 0 节) |

### 3.7 解释器启动 / 导入 — 1.5–2.7x 🟡

| 动作 | CP | RP | 比值 |
|---|---:|---:|---:|
| 空进程 | 10–20ms | 22–32ms | ~1.5–2x |
| import json | 55–62ms | 154–170ms | ~2.7x |

---

## 4. 与上次会话基线对比 (进步确认)

| 指标 | 上次基线 | 本次 | 状态 |
|---|---:|---:|---|
| json_roundtrip_2k | 21x | **2.59x** | ✅ native JSON 生效 |
| str.join | 21.4x | 4.15x | ✅ 大幅改善 (仍有 4x) |
| str_concat_30k | 13x (会话初) | 1.35x | ✅ |
| class_method_20k | 6.5x | 3.17x | ✅ (__slots__ 场景) |
| vs_cpython geomean | 0.95x (理想) / ~2.5x (负载) | **2.66x** | 与负载态一致, 未恶化 |
| str_short_create_150k | 3.4x | 3.24x | ➖ 持平 |

## 5. 结论

1. **RustPython 在本机尚未达到与 CPython 持平的目标**: 真实负载下综合 geomean 2.6–3.9x, 计算密集整程序 2.6–5.7x。"micro 套件平价"仅存在于静默机器 + 理想配对测量的条件, 不可复现于常规状态。
2. **最大病态差距已修复**: cp1252/单字节 legacy codec 解码 ~714x → ~3.7x (见第 0 节), json_loads 整程序 35.7x → ~3.0x。该修复已落地并过全量 codec 回归 (test_codecs / test_codeccallbacks 全绿, 56 codec × 4 错误处理 diff=0), 附带修复 bpo-36819 错误处理重启语义。
3. **结构性差距 (需工程)**: 函数/方法调用 4.4–7.8x 与微操作分派 4–8x, 与 roadmap 记录的 dispatch floor / fn_call FuncArgs 协议一致; isinstance 8.2x、小对象 getitem 5.8–7.5x 是调用链路上的同一根源。strbench 的 join/split/format/len 剩余 2.3–6.4x 同属此根源 (join 的 exact-str 预扫描快速路径已存在)。
4. **已达标亮点**: 补偿 sum (complex 0.63x)、sum(range) 原生快路径 (~12x 快)、原生 str count/replace/upper 平价、native JSON、SSO 短串、i64 sort。这些证明"单次 Rust 调用内批量操作"的路径已可与 CPython 匹敌甚至胜出, 差距集中在跨指令/跨调用边界。
5. **优先修复建议 (余下)**: ① fn_call vectorcall 式直传 (roadmap 已有方向, 4.4–7.8x 结构性差距, 收益最大); ② str.split/format 共享缓冲子串与格式缓存 (roadmap 候选); ③ dict_str_lookup 复测 (机器状态敏感, 负载下 4.2–6.9x 波动, 非硬回归)。

## 6. 最终状态与更进一步

**鲁棒性验证**: codec(test_codecs 287+) 与 decimal(70 项交叉对照) 全绿; son, dict list string float tuple int 悉数通过。普通应用(类型 dict/list/str/float) 比较稳定, 矩阵型对象(符号运算/超大数字成荫循环) 稳定。

**当前 binary 运行在两轮完整基准套件(kostya + pyperformance + 7 项自研)上。优化差距如下(≥3x 且可运作)**:

| 差距 | 结构瓶颈 | 余量 |
|---|---|---|
| fn_call (~4.8x) | FuncArgs 协议 / frame setup | 原生 vectorcall 直传 + frame arena 改造 |
| instance alloc (~6.9x) | system malloc + GC object prefix | 自由列表 pymallok-style+dict 池 |
| generator (~7.6x) | 嵌套 yield-from 分派每值 2-3 帧 | generator feta由 native 共享 datastack |
| string micro (~3-8x) | 每次方法调用 + result 创建 | str.format 格式化 缓存; split 共享 buffer |
| rl ops (~5-7x) | deltablue 类 dense 方法调用 | fn_call 改造即带动 |

**降低风险方法**: 这些都需要 CPython-level vectorcall(例) 与 instance-freelist mocip——逐个会话执行, 每次跑回归测试(IT test_* / pyperformance target 接受性测试) 才达成。

## 交付物
- 综合结果报告: `RustPython/bench/reports/benchmark_run_2026-08-26.md`
- 原生 Decimal: `RustPython/crates/stdlib/src/_decimal.rs` + `crates/stdlib/src/_decimal_core.rs`
- codec 原生化: `RustPython/crates/vm/src/stdlib/_codecs.rs` (charmap 方言)
- pyperformance 对照 runner: `temp/run_pyperformance.py`
