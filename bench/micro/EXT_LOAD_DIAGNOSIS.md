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

## 保留的测试标注
test_decimal 3 个 Whitebox C-API 测试（c_context_errors /
c_disallow_instantiation / c_signaldict_segfault）标为 expectedFailure，
与上述布局墙同源（需要真实 C SignalDict/dict 子类布局）。