# 原生扩展加载链三阶段诊断（numpy / _cffi_backend）

## 阶段 1：DLL 加载（error 126）— ✅ 已修复
- 根因：`numpy.libs` 未进入 DLL 搜索路径
- 修复：`os.add_dll_directory`（numpy 自带 delvewheel patch 已处理）

## 阶段 2：符号解析（error 127）— ✅ 已修复
- 根因：`rustpythonapi.dll` 缺失 C-API 导出符号
- 修复：补齐 20+ 符号（`PyUnicode_AsWideCharString`, `PyThreadState_GetDict/GetInterpreter/GetUnchecked`,
  `PyGILState_GetThisThreadState`, `Py_FatalError`, `PyErr_FormatUnraisable`, `PyType_GenericAlloc`,
  `PyRun_StringFlags`, `_PyLong_Sign`, `_PyObject_GC_NewVar`, `_Py_FatalErrorFunc`,
  `PyModule_Type`, `PyByteArray_Type`, `PyExc_WindowsError`, `Py_HashPointer` 等）
- 验证：`_cffi_backend` 从 "cannot load extension module" → 进入 PyInit（ImportError 变化可证）
- shim 导出从 1033 → 1055

## 阶段 3：PyInit / 布局兼容（架构墙）— ⛔ 未解决
- `_cffi_backend`：PyInit 内部版本检查读取 `sys.version` 实例 ob_type 的
  tp_flags@0xa8，要求 Py_TPFLAGS_UNICODE_SUBCLASS；RustPython 真实 str 类型
  布局不匹配（已尝试给 stub 补标志，无效——实例 ob_type 指向真实类型）
- numpy：`PyList_SET_ITEM` 内联宏按 CPython PyListObject 布局写 ob_item@0x18，
  得到 RustPython 原生 PyList → 写地址 0x1 → AV

## 判定
阶段 1/2 是**实现有问题（可修复）**，已全部修复。
阶段 3 是**架构性分派地板**：官方 CPython ABI wheel 的宏/字段访问按
CPython 内存布局硬编码，RustPython 对象/类型布局不同。修复需 foreign
raw-buffer 对象（PyList/PyTuple/PyBytes/类型），是 PyPy cpyext 级别的
大工程（子代理 B 已给出方向 A 方案，工程量大）。

## 方向 A（PyListObject foreign 布局）可行性验证（子代理 abc7b13f 完成）

**布局实证**：CPython PyListObject = ob_refcnt@0/ob_type@8/ob_size@0x10/**ob_item@0x18**；
RustPython PyInner = ref@0/typ@8/payload@0x10（头仅 0x10 字节）。numpy 内联宏
读 `[r+0x18]` 落在原生 payload（RwLock<Vec>）内部 → 崩溃根因确认。

**Probe 验证：PASS（3/3）**（`crates/capi/tests/foreign_list_probe.rs`，自包含
仅依赖 libc，未改任何现有文件）：malloc foreign 缓冲 + ob_item 槽数组 → 手写
CPython 头 → 模拟 `mov rax,[r+0x18]; [rax]=rcx` → 读写一致，头字节完整。

**结论**：方向 A 内存机制**可行**；完整修复需大工程（12 个 PyList_* 导出函数
双路径 + dealloc + PyList_Check + Python 层 list 协议——现有 foreign_dispatch
仅覆盖 getattr/call）。推荐折中：**边界物化**——wrap_foreign 对 list-stub 型
缓冲重建原生 PyList（数组存活至 refcount 归零），覆盖 numpy"构建一次即返回"
模式，改动面小得多。

**边界物化实施探索（2026-09-03，子代理 bce8271f，已中断）**：
- 尝试实施：PyList_New 返回 CPython 布局缓冲 + wrap_foreign list 物化 +
  PyList_* 双路径，触及 capi 5 文件 + vm/object/core.rs（GC/refcount 核心）
- 结果：4 轮验证迭代（out/err 1-4）未收敛到可提交状态——实现触及
  `is_foreign` 判定与 GC 引用计数平衡，复杂度超出单次子代理能力
- 判定：**方向 A 完整实施需专职工程**（capi 双路径 + GC 语义），不可在
  单次并行子代理中收敛。可行性已由 probe 3/3 PASS 证实，实施计划已文档化
  （NUMPY_DIRECTION_A_PLAN.md），留待专职大工程推进。

## 额外发现：test_code 模块崩溃（2026-09-03，预先存在，与本次优化无关）
- 现象：`-m test test_code` 模块级崩溃（AttributeError: GetProcAddress failed）
- 根因：库模块顶层 `ctypes.pythonapi.PyUnstable_Eval_RequestCodeExtraIndex`
  缺失——RustPython 的 check_impl_detail(cpython=True) 返回 True（自报为
  CPython），但 capi 未导出 CPython 不稳定 Eval/Code extra-index API
  （PyUnstable_Eval_RequestCodeExtraIndex / PyUnstable_Code_SetExtra /
  PyUnstable_Code_GetExtra）
- 判定：预先存在（test.support 与 test_code.py 均未在本会话修改），
  与函数创建优化无关；test_builtin 独立运行 SUCCESS。若要修复需在 capi
  实现不稳定 Eval/Code extra 索引 API（映射到 code 对象的 extra 槽位），
  属 capi 补齐项，可单独排期

## 标准库测试模块扫描（2026-09-03）
对 10 个标准库测试模块（test_code/capi/sys/json/math/functools/inspect/
types/contextlib/dataclasses）做 import 扫描：**仅 test_code 存在
ctypes/GetProcAddress 类崩溃（capi 符号缺口）**，test_capi 为模块不存在
（环境无此文件，非符号缺口），其余 8 个模块全部可正常加载。
→ 确认该 capi 缺口是孤立的（不扩散到其他标准库模块）。

## code-extra 实施探索（2026-09-03，子代理 df8f8d3e，已中断）
- 尝试：在 PyCode 增加 extra 侧表 + 全局 freefunc 注册表 + 三个导出符号
- 结果：子代理运行约 16 分钟无任何产出（无文件改动/构建/日志），
  疑似在 PyCode 析构/GC 回收时机方案上停滞，中断处理
- 判定：实现需在 VM 核心 PyCode 结构加侧表并解决回收时机（freefunc 调用
  与 GC/refcount 交互），属中等工程、需专职实施；保持为可排期 capi 补齐项
- 注意：RustPython 的 check_impl_detail(cpython=True) 返回 True 本身也值得
  复核——若改回正确返回 False，test_code 的 ctypes 块会整体跳过，
  这是修复该崩溃的**更小方案**（改 test.support 的 impl 判定或支持层）

## sys.implementation.name 判定（2026-09-03，不实施）
- 根因：`crates/vm/src/stdlib/sys.rs:671` 硬编码 NAME="cpython"（注释：cache
  tag 用 cpython 因编译器兼容），`_sysconfig.rs:26` 注释说明保持 cpython
  让 pip 可找到 wheel——**有意设计**，非缺陷
- 判定：改为 "rustpython" 会破坏 pip 兼容（wheel 平台标签）与大量依赖
  check_impl_detail(cpython=True) 的 CPython 兼容测试 → **不实施**。
  因此 test_code 崩溃的可行修复路径仅剩：实现 unstable Eval/Code
  extra-index capi（中等工程，PyCode 加侧表 + freefunc 回收时机），
  或对 test_code 标注 skip/expectedFailure（需谨慎——模块级崩溃使
  expectedFailure 无效，需按 crash 处理用 skip）

## 保留的测试标注
test_decimal 3 个 Whitebox C-API 测试（c_context_errors /
c_disallow_instantiation / c_signaldict_segfault）标为 expectedFailure，
与上述布局墙同源（需要真实 C SignalDict/dict 子类布局）。