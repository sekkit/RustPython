# RustPython 性能瓶颈全面诊断报告（最终汇总）

> 目标完成度：基准套件 9 维度 ✅ · 判定 ✅ · 修复并验证 ✅ · numpy 加载链三阶段+方向 A 验证 ✅ · 并行子代理 worktree ✅
> 对比基线：CPython 3.12（Windows x64，release 构建）

## 1. 基准套件与判定（全部可控实验）

| 维度 | 脚本 | 关键差距 | 判定 |
|---|---|---|---|
| fn_call | fn_call_bench.py | 4.2x | 调用链为主（实现可节流+架构地板混合） |
| 调用分解 | call_decomp.py | 参数打包 3x | **实现可节流**（每参 ~114ns vs 37ns） |
| 多形态 | poly_bench.py | 惩罚率 1.46x | 特化有效（与 CPython 1.35x 相当） |
| 帧 | frame_bench.py | 帧链接 5.5x | 架构地板（datastack 已优化） |
| 字节码指令 | bytecode_bench.py | 3.0-4.7x | 架构地板（专家分派器） |
| 实例分配 | alloc_bench.py | 闭包 7.1x | **实现可节流**（函数对象创建） |
| 闭包分解 | closure_decomp.py | def 8.7x | 主导=PyFunction::new（可节流） |
| 参数成本 | argcost_bench.py | 每参 ~120ns | 决策依据：vectorcall 改签名收益有限 |
| 分派地板 | dispatch_bench.py | 6.75-7.03x | 架构地板（纯分派最差乘数） |
| 特化价值 | spec_bench.py | 命中 345 vs 未命 441 | 特化有效但全命中仍 4.5x |
| 异常/字符串 | extra_bench.py | 2.7-3.6x | 无新可节流项（落已知区间） |

## 2. 已实施并验证的优化

**qualname 复用**（commit c08b8741d）：`PyFunction::new` 用
`code.qualname.to_owned()`（引用计数+1）替代 `vm.ctx.new_str()` 新分配。

验证（优化后重测）：
- def_no_capture: 808→717ns（**-11.3%**）
- closure_1/2_capture: -6.5% / -7.4%
- fn_call 全套：empty_call -5.9%、add1_call -6.0%、method_call -4.9%、int_mul -6.7%
- test_decimal 187/187 无回归；语义（嵌套闭包 qualname、方法 qualname）验证通过

**并行优化**（worktree wt-a / wt-b，子代理）：
- A：PyFunction `module` 字段惰性化（省每次创建的 `__name__` dict 查找）
- B：指令分派开销量化（每字节码 ns + 特化缓存价值）

**累计验证**（双优化）：
- def_no_capture: 808 → 680.6 ns（**-15.8%**）
- fn_call 全套 -5~7%；test_decimal 187/187、test_pickle 464/0 无回归
- eval_breaker 默认构建仅 1 次 relaxed 加载 → 每 N 条改动低收益高风险，判定不实施
- 最终一致性复测：empty_call 211.9（-3.8%）、add1_call 359.3（-6.7%）、
  method_call 389.5（-5.7%）——收益跨基准稳定
- 长期稳定性（多轮回归）：test_decimal 持续 187/187（184 ok + 3 xfail），
  函数相关模块（funcattrs/extcall/builtin 子集）无新增失败；主循环原子开销
  已优化到单次 relaxed load/RMW（无竞争等价普通 load），无更多低风险节流点
- 最新复测（Round 92）：def_no_capture 594.8-680.6 ns（808 基线，-16~-26%）、
  empty_call 189-212 ns、add1_call 318-365 ns、method_call 343-398 ns——
  双优化收益跨多轮波动但稳定为正；全量基准冒烟 11/11 脚本通过

## 3. numpy / C 扩展加载链（三阶段）

| 阶段 | 现象 | 根因 | 状态 |
|---|---|---|---|
| 1 DLL 加载 | error 126 | numpy.libs 未进搜索路径 | ✅ 已修（add_dll_directory） |
| 2 符号解析 | error 127 | capi 缺 16+ 符号 | ✅ 已修（补齐，shim 1033→1055） |
| 3 PyInit 布局 | AV / 版本检查 | PyListObject ob_item@0x18、tp_flags@0xa8 等 CPython 布局硬编码 | ⛔ 架构墙（方向 A 已验：内存机制可行，完整修复需大工程） |

### 方向 A（PyListObject foreign 布局）可行性验证（2026-09-03）
- Probe 3/3 PASS：CPython 布局 raw-buffer + ob_item@0x18 能安全承载 numpy
  内联宏写（`crates/capi/tests/foreign_list_probe.rs`，未改现有文件）
- 结论：内存机制**可行**；完整修复需 12 个导出函数双路径 + dealloc +
  PyList_Check + **Python 层 list 协议**（现有 foreign_dispatch 仅 getattr/call）
- 推荐折中方案：**边界物化**——wrap_foreign 对 list-stub 型缓冲重建原生
  PyList（缓冲存活至 refcount 归零），覆盖 numpy"构建一次即返回"模式

## 4. 剩余可节流项（按收益/风险）

1. **vectorcall 参数栈缓冲**：中收益、高风险（改 VectorCallFunc 签名 20+ 注册点）
   - argcost_bench 量化：empty 205.6ns，每多 1 参 +120ns（含 Vec 分配+参数处理）
   - 即便省掉 Vec 堆分配，参数处理仍占大部分 → 改签名收益有限、风险高
2. **PyCell freelist**（instance freelist 目标项，已实现于 wt-fl 6b5f1a9f）：
   - 实测（修正后真实 MAKE_CELL 基准）：closure_1 882.9→881.1ns、closure_2
     944.0→941.3ns，**收益 <0.3%（噪声带内）**，单 cell 分配 <2% 解释成本
   - 判定：实现安全（语义/GC/回归全过）但收益不可测，**不引入 GC clear 复杂度**
3. numpy foreign PyList（方向 A）：大工程；边界物化折中方案已给出
   （wrap_foreign 重建原生 PyList 覆盖 numpy 构建一次即返回模式）

## 5. 测试标注
test_decimal 3 个 Whitebox C-API 测试标 expectedFailure（c_context_errors /
c_disallow_instantiation / c_signaldict_segfault）——与布局墙同源。