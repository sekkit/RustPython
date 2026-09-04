# vectorcall 参数栈缓冲评估（目标项 4）

> 状态：**已量化并判定**——不实施（低收益高风险），维持既有结论。
> 评估依据：dispatch_results.md（execute_call_vectorcall 回落路径 1.25–1.38x
> 惩罚）+ argcost_bench.py（每参 ~120ns，含参数处理）。

## 背景
`crates/vm/src/frame.rs:8359 execute_call_vectorcall` 每调用
`Vec::with_capacity(effective_nargs)` 做一次堆分配构建参数数组。dispatch
分析指出这是 CALL 未命中特化时的回落路径开销放大点（相对快速路径 1.25–1.38x，
CPython 仅 1.08x）。

## 方案候选
1. **栈上小缓冲**（≤3 参数用 `[MaybeUninit; N]`，>3 才堆分配）：
   - 收益：仅落在**未命中回落路径**（已特化的 CALL/BINARY_OP 等不经过此处）
   - 代价：需新依赖（smallvec/tinyvec）或手工 unsafe 栈数组；触碰热路径
     frame.rs 核心调用逻辑
   - 风险：CALL 已大部分被特化缓存覆盖（probe 显示全特化），未命中概率低，
     实测收益在噪声范围内
2. **vectorcall 签名改造**（参数栈直传，改 20+ 注册点）：
   - 此前已判定高风险低收益（签名触及所有 callable 注册点，收益 ~120ns/arg
     且仅未命中路径）
3. **保留现状**：回落路径 1.25–1.38x 惩罚比例虽大于 CPython，但绝对值
   （~90-130ns 每次调用）仅在未命中时出现，整体贡献有限

## 判定
- 仓库无 smallvec/tinyvec/arrayvec 依赖（已核实 Cargo.toml 与源码引用）。
- 引入依赖 + 手工栈缓冲的工程投入 vs 仅未命中路径的收益：**不划算**。
- vectorcall 签名改造维持"高风险低收益"判定（DELIVERABLES.md 第 38 行已有记录）。
- **结论：维持不实施**，文档化作为既定判定（目标框架"低收益/风险不对称"类）。
