# numpy 方向 A 实施计划：边界物化（Boundary Materialization）

> 目标：让 numpy 官方 CPython ABI wheel 能完成 `import numpy`（当前卡在
> PyList_SET_ITEM 内联宏写 ob_item@0x18 崩溃，架构墙）。
> 前置已验证：`crates/capi/tests/foreign_list_probe.rs` 3/3 PASS——
> CPython 布局 raw-buffer + ob_item@0x18 能安全承载内联宏写。

## 方案选型：边界物化（vs 全量双路径）

**全量双路径**（12 个 PyList_* 导出函数都支持 foreign 布局）工程量太大：
还需 PyList_Check 分支、list 专用析构、以及**Python 层 list 协议**
（现有 foreign_dispatch 仅覆盖 getattr/call——numpy 返回的 foreign list
经 wrap_foreign 后 len/索引/迭代/contains 全不可用）。

**边界物化**（选此方案）：wrap_foreign 在识别到 list-stub 型 foreign 缓冲时，
用 ob_item 数组**重建原生 PyList**，外外部保持 CPython 语义。覆盖 numpy
"构建一次即返回"的典型模式（numpy 构建内部 list 后通常不再通过 C 侧改它）。

## 实施步骤（按依赖排序）

### 步骤 1：wrap_foreign 的 list 特化（核心）
- 文件：`crates/capi/src/foreign.rs`（`wrap_foreign_object`，约 349 行）
- 逻辑：`resolve_type_ptr` 得到类型后，若该类型是 `list_type`（且原缓冲
  ob_type@8 是 PyList_Type stub）→ 从 ob_item 数组逐槽 incref 重建
  `vm.ctx.new_list(Vec<PyObjectRef>)`；缓冲本身由调用方持有至 refcount 归零
- 关键：元素是 RustPython 对象指针（numpy 存入的 PyUnicode_FromString 结果），
  incref 后进原生 Vec；原缓冲保留（foreign 注册表管理），dealloc 时
  `libc::free` 数组与头（不 decref 元素，避免双释放）

### 步骤 2：PyList_New 返回 CPython 布局缓冲（触发点）
- 文件：`crates/capi/src/listobject.rs:15-22`（`PyList_New`）
- 改为：libc::malloc 分配 CPython PyListObject 头（0x28）+ ob_item 数组
  （size×8），手写 ob_refcnt=1@0 / ob_type=list_stub@8 / ob_size=size@0x10 /
  ob_item=array@0x18，`register_foreign_object` 注册
- 所有现有 PyList_* 导出函数对 foreign list 仍走 downcast 失败路径——
  需要**最小双路径**：PyList_Size/GetItemRef/SetItem/Append 加 foreign 分支
  （直接操作 ob_item 数组）

### 步骤 3：版本检查（_cffi_backend 报告过的 tp_flags@0xa8）
- `PyUnicode_Type` stub 已补 Py_TPFLAGS_UNICODE_SUBCLASS（commit a8b8634b9）
- 仍需确认真实 str 类型实例的 ob_type 指向 stub 而非真实类型（cffi 崩溃点）

### 步骤 4：验证
1. `import numpy` 复现：越过 PyInit 崩溃 → 到下一个关卡（会暴露后续布局问题）
2. `_cffi_backend` 完整加载
3. test_decimal 187/187 无回归、capi 导出 1055 不变
4. 简单 C 扩展（手写 PyList_New + PyList_Append + 读回）功能验证

## 风险与回退
- 高风险：PyList_* 双路径若遗漏某函数会让其他扩展拿到 foreign list 后
  downcast 失败 → 需逐函数验证；若波及过大，回退封在 `#ifdef` 或按
  `register_foreign_object` 门控
- GC：capi 侧为引用计数模式，手工 incref/decref 需与原生 Vec 平衡
- 替代：若边界物化仍不覆盖 numpy 实际使用模式（返回后继续 C 侧改 list），
  退回全量双路径（12 函数 + dealloc + list 协议适配器）

## 工作量估算
- 步骤 1：~80 行（foreign.rs 特化）
- 步骤 2：~60 行（PyList_New + 4 个高频函数 foreign 分支）
- 步骤 3：~20 行（版本/类型 stub 核对）
- 步骤 4：验证循环 2-3 次构建（每次 5-10 分钟）
- 总：中-大工程，建议独立 worktree 实施，主仓保持稳定