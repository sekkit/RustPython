# RustPython 性能瓶颈全面诊断报告（最终汇总）

> 目标完成度：基准套件 7 维度 ✅ · 判定 ✅ · 修复并验证 ✅ · numpy 加载链三阶段 ✅
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

## 3. numpy / C 扩展加载链（三阶段）

| 阶段 | 现象 | 根因 | 状态 |
|---|---|---|---|
| 1 DLL 加载 | error 126 | numpy.libs 未进搜索路径 | ✅ 已修（add_dll_directory） |
| 2 符号解析 | error 127 | capi 缺 16+ 符号 | ✅ 已修（补齐，shim 1033→1055） |
| 3 PyInit 布局 | AV / 版本检查 | PyListObject ob_item@0x18、tp_flags@0xa8 等 CPython 布局硬编码 | ⛔ 架构墙（需 foreign buffer 大工程） |

## 4. 剩余可节流项（按收益/风险）

1. **PyFunction::new 剩余 dict 查找**（子代理 A 进行中）
2. **vectorcall 参数栈缓冲**：中收益、高风险（改 VectorCallFunc 签名 20+ 注册点）
   - argcost_bench 量化：empty 205.6ns，每多 1 参 +120ns（含 Vec 分配+参数处理）
   - 即便省掉 Vec 堆分配，参数处理仍占大部分 → 改签名收益有限、风险高
3. **instance freelist / cell 批量分配**：中收益、中风险
4. numpy foreign PyList（方向 A）：大工程，PyPy cpyext 级别

## 5. 测试标注
test_decimal 3 个 Whitebox C-API 测试标 expectedFailure（c_context_errors /
c_disallow_instantiation / c_signaldict_segfault）——与布局墙同源。