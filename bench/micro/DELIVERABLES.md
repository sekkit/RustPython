# RustPython 性能诊断最终交付清单（Round 124）

## 交付物总览（主仓 clean，27 文件，17715 commits）

### 基准脚本（13）
fn_call_bench.py / call_decomp.py / poly_bench.py / frame_bench.py /
bytecode_bench.py / alloc_bench.py / closure_decomp.py / argcost_bench.py /
dispatch_bench.py / spec_bench.py / extra_bench.py / extra2_bench.py

### 结果文档（14）
PERF_SUMMARY.md / PERFORMANCE_ANALYSIS.md / FINAL_REPORT.md /
fn_call_results.md / call_decomp_results.md / bytecode_results.md /
alloc_results.md / closure_results.md / dispatch_results.md /
EXT_LOAD_DIAGNOSIS.md / NUMPY_DIRECTION_A_PLAN.md / extra_results.md /
extra2_results.md / DELIVERABLES.md（本清单）

## 关键成果

1. **test_decimal 187/187**（19.3%→100%）——38 值不匹配 case、from_number/
   IEEEContext/pickle、locale 格式化全部修复；多轮回归稳定

2. **性能诊断（11 维度，全部判定）**：
   - 指令级：bytecode（3.0-4.7x，架构地板）、dispatch 地板（6.75-7.03x）、spec（特化有效）
   - 调用分解：fn_call（4.2x）、参数打包（每参 114ns vs 37ns，3x 可节流）、argcost
   - 实例分配：alloc（闭包 7.1x）、closure 分解（def 8.7x → 函数创建主导）
   - 分派：poly（1.46x vs 1.35x）、frame（5.5x）、extra（异常/字符串 2.7-3.6x）

3. **修复实施并验证**：qualname 复用（-11.3%）+ 惰性 module（-8%），
   def 创建累计 -26%（808→595ns 实测），fn_call -13~-17%，无回归

4. **numpy/C 扩展**：三阶段诊断（126/127 修复 [+20 符号]、布局墙架构性）、
   方向 A 可行性 probe 3/3 PASS、边界物化实施计划文档化

5. **多子代理并行**：7 子代理（2 优化、2 基准、3 诊断），worktree 模式验证可行

## 判定汇总
- **实现可节流（已修）**：函数创建分配（qualname/module 惰性）
- **实现可节流（方案已给）**：vectorcall 参数栈缓冲（高风险低收益，argcost 量化）
- **架构地板**：分派（6.75-7.03x）、帧（5.5x）、多态（1.46x）、字节码（3-4.7x）
- **架构墙**：numpy 布局（PyListObject ob_item@0x18，需 cpyext 级大工程）、
  instance freelist（实测 <0.3% 不引入）