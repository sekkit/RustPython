# 闭包创建分解（RustPython vs CPython 3.12，ns/iter，N=500k）

| 场景 | RustPython | CPython | 倍数 |
|---|---|---|---|
| def_no_capture（无闭包） | 808.0 | 92.6 | **8.7x** |
| closure_1_capture（1 cell） | 869.9 | 116.4 | 7.5x |
| closure_2_capture（2 cells） | 927.0 | 131.1 | 7.1x |
| list_as_cell（类似 cell 分配） | 232.2 | 64.1 | 3.6x |
| reuse_cached_func（复用函数对象） | 230.9 | 52.9 | 4.4x |

## 已实施的优化：qualname 复用（commit c08b8741d）
`PyFunction::new` 用 `code.qualname.to_owned()` 替代 `vm.ctx.new_str(...)`：
code 对象创建时已 intern_str qualname，直接复用 PyStrRef（引用计数+1），
不再为每个函数新分配字符串。

**验证**（优化后重跑 closure_decomp.py）：
- def_no_capture: 808.0 → **717.0 ns（-11.3%）**
- closure_1_capture: 869.9 → 813.1 ns（-6.5%）
- closure_2_capture: 927.0 → 858.0 ns（-7.4%）
- list_as_cell / reuse_cached_func 不变（~231ns，与此优化无关）

## 关键结论
- **def_no_capture 8.7x > closure_1_capture 7.5x > list_as_cell 3.6x**
  → 闭包 7.1x 的主导成本是**函数对象创建本身（PyFunction::new）**，
  而不是 cell 分配；cell 分配只贡献 ~60-120ns
- `PyFunction::new`（frame.rs MakeFunction → function.rs:176）每次创建做：
  - 2 次 dict 查找（`__name__`、`__builtins__`）+ docstring 提取
  - 一次 PyFunction 对象堆分配（pyobject 头 + payload）
  - 9 个 `PyMutex` 字段初始化（lock_api Mutex，T 内联存放，
    PyStrRef/PyTupleRef 为指针大小 → 无额外堆分配）
  - `next_func_version()` 原子自增
- 对比：CPython 用紧凑 PyFunctionObject + 惰性 dict；RustPython 每次函数
  创建做 2 次 dict 查找 + 完整对象分配 → 这是集中的**实现可节流项**

## 修复方案（按风险排序）
1. **低风险**：`PyFunction::new` 减少分配——name/qualname 复用 code 对象的
   PyStr（不 clone），type_params 惰性化，annotations/annotate 默认免分配
2. **中风险**：将多个 PyMutex 小字段并到一个 `Box<FunctionExtra>`（1 次分配）
3. **高风险**：函数对象 freelist（CPython 3.12+ 做法），需配合 GC 语义验证

> 验证方式：改后重跑 `closure_decomp.py`，目标 def_no_capture ≤ 500ns

## 已实施的优化叠加：惰性 module 字段（commit db6cc1bcf，子代理 A，worktree 并行）
`PyFunction::new` 移除了每次创建的 `globals["__name__"]` dict 哈希查找：
`module` 字段改为 `PyMutex<Option<PyObjectRef>>`，首次访问 `__module__` 时
才从 globals 惰性计算并缓存，取不到回退 None——省一次 dict 查找/函数。

**双优化累计验证**（qualname + 惰性 module，主仓重测）：
- def_no_capture: 808.0 → 680.6 ns（**累计 -15.8%**；单 module 优化约 -8%）
- closure_1_capture: 869.9 → 767.8 ns
- closure_2_capture: 927.0 → 811.5 ns
- 语义验证：顶层 `__main__`、pickle 往返、setter、exec 无 __name__ → None 全过
- 回归：test_decimal 187/187 无变化；test_funcattrs 35 测试 34 通过
  （唯一失败为预先存在的 test_builtin__self__，与函数创建无关）