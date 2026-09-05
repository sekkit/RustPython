# numpy 方向 A 边界物化 — 实施进展记录

> 状态：**第一阶段完成并验证**。numpy 从 AV 崩溃连续推进三关（越过 PyList 布局墙）。
> 三个修复已合并到主仓（b559bdbc8 / 8e6e27e30 / 0aa793e3e）。

## 里程碑：越过 PyListObject ob_item@0x18 布局墙

### 修复 1：边界物化（commit b559bdbc8）
- `crates/capi/src/listobject.rs`：
  - `PyList_New` 现在返回 **CPython 布局 foreign 缓冲**（ob_refcnt@0 / ob_type@8 /
    ob_size@0x10 / ob_item@0x18 / allocated@0x20），通过 `new_foreign_list` 分配，
    注册为 foreign。numpy 的内联 `PyList_SET_ITEM` 宏写 ob_item@0x18 不再崩溃。
  - 新增 `foreign_list_to_native`（物化：逐槽 incref 重建原生 PyList）与
    `is_foreign_list_buffer`（识别 CPython 布局 list 缓冲）。
  - `PyList_Size / PyList_GetItemRef / PyList_SetItem / PyList_Append` 增加
    **foreign 双路径分支**（直接操作 ob_item 数组 / ob_size）。
  - 新增 `PyList_NewForeign`（显式创建，工具/测试入口）。
- `crates/capi/src/foreign.rs`：`wrap_foreign_object` 对 foreign list 缓冲走物化
  分支（重建为原生 PyList），其余对象维持原逻辑。
- `crates/capi/src/refcount.rs`：`_Py_Dealloc` 对 foreign list 缓冲走专用释放
  （先 unregister，再逐槽 decref 元素、free ob_item 数组与头），防泄漏/双释放。
- `bench/verify_materialize.py`：验证脚本 —— 内联写 ob_item[0] 无 AV、
  PyList_Size/GetItemRef 读回正确（FOREIGN_LIST_DUAL_PATH_OK）。

**验证**：import numpy 从 "AV 崩溃（PyInit 阶段）" 推进到
`TypeError: unhashable type: ''`——PyInit 已完成，原崩溃点越过。

### 修复 2：空 hash slot = 身份哈希（commit 8e6e27e30）
- `crates/vm/src/protocol/object.rs`：`PyObject::hash` 在 `slots.hash` 为空时
  回退 `get_id()` 身份哈希（CPython 语义：仅显式 `tp_hash=NULL`/`__hash__=None`
  的类型不可哈希，内建 unhashable 由 `unhashable` pyclass 标记走
  `hash_not_implemented` slot；空 slot 意味着默认身份哈希）。
- 影响：numpy heap type 实例（C 扩展创建）可按身份哈希，不再误报 unhashable。

**验证**：numpy 推进到 `AttributeError: '\x00' object has no attribute '__name__'`
（numpy dtypes._add_dtype_helper 处，DType 类对象 name 损坏）。

### 修复 3：getattro 空 slot 回退（commit 0aa793e3e）
- `crates/vm/src/protocol/object.rs`：`get_attr_inner` 在 `slots.getattro` 为空时
  回退 `object.__getattribute__`（`PyBaseObject::getattro` 即 `generic_getattr`），
  不再 unwrap panic。

**验证**：numpy 推进到 `AttributeError: '\x00' object has no attribute '__name__'`
（不再 panic，错误可捕获）。

## 第四关：DType __name__（已修复，commit 6ad759994）
- 现象：`AttributeError: '\x00' object has no attribute '__name__'`
  触发：numpy `_core/dtypes.py:80` `_add_dtype_helper(DType, alias)` 的
  `DType.__name__` 访问。
- 根因（字节级证据链）：numpy 2.5.2 的内建 DType 类**不是** PyType_FromSpec
  创建，而是编译进 `_multiarray_umath.pyd` 的**静态 CPython PyTypeObject 结构体**
  （pyd 导入表只含 PyType_Ready / PyType_GenericNew / PyType_Type）；ob_type@8
  指向 DLL 内静态 metaclass（PyArray_DTypeMeta_Type），tp_name@24 可读、vtable=0。
  vm 的 get_attr_inner 把裸结构体当 RustPython PyType，name 读出 "\0"。
- 关键发现：exe 启动走 `init_main_interpreter`（src/lib.rs:136）而非
  `Py_InitializeEx`，`foreign_dispatch::set()` 此前从未在二进制安装（is_foreign
  恒 false、wrap_foreign 恒 None）——前三个 commit 的 foreign 机制在 exe 中
  部分休眠；本修复在 init_main_interpreter 补装整个 foreign 调度表 + 物化回调。
- 修复：`materialize_cpy_type_object` 回调（capi foreign.rs）识别裸 CPython
  type 结构体（ob_type@8 与 tp_name@24 都在非 exe 模块、tp_name 有界可读），
  用 tp_name 物化成原生 heap type 并按地址缓存（实测读出
  `tp_name="numpy.dtypes.BoolDType"`）；`get_attr_inner` 两分支尝试物化；
  `PyMethod::get` getattro 空 slot 不再硬 unwrap；init_main_interpreter +
  Py_InitializeEx 注册全部 5 个回调。
- 验证：dtypes.py:80 的 __name__ AttributeError 消失（0 命中）；numpy 推进到
  下一关——`_multiarray_umath` init 期间 `ExecutingFrame::load_attr` 的
  NATIVE-CRASH access violation（类型层级语义缺失）。
- 下一关精确缺失点（未修复，建议专职推进）：
  1. 将"静态 CPython type 结构体物化"推广到 resolve_type_ptr 盲 fallback、
     PyType_GenericNew/GenericAlloc、_PyObject_New/GC_New 返回的 type 对象；
  2. 物化时从 tp_base（offset 256）递归物化 MRO，并设置 __module__；
  3. crash_diag 补当前 Python 帧（file:line）定位原生 AV 源码位置。

## 第五关：LOAD_ATTR AV（已修复，commit 92d761431）
- 现象：`_multiarray_umath` init 注册首个 DType 后，
  `ExecutingFrame::load_attr` NATIVE-CRASH（rcx=裸 CPython 对象、rax 垃圾值）。
- 根因：`specialize_load_attr`（load_attr 包装内、slow 路径之前）直接
  `obj.class()`——对裸 CPython 布局对象（numpy 经 C 参数打包直接进帧、未注册
  foreign）读取 CPython 头部字节当 RustPython PyType，崩溃。`get_attr_inner`
  已有物化处理，但 specialize 抢先崩溃无法到达。
- 修复：specialize_load_attr 开头探测物化回调（其内部对非 type 对象安全返回
  NULL）——is_foreign 或物化命中即跳过特化、回退 slow 路径（slow 的
  get_attr_inner 尝试物化）。
- 验证：LOAD_ATTR AV 越过；numpy 推进到第六关——Rust std
  `windows::decode_error_kind` 崩溃（rdx=垃圾值被当 Windows os error 码）。

## 第六关：decode_error_kind 崩溃（判定：待专职）
- 现象：`std::sys::io::error::windows::decode_error_kind` (read) 0x10 AV，
  rdx=0xbd95b944f0 垃圾值被当错误码。
- 性质：numpy 某 Windows API 路径（console/io/编码检测）产生的错误状态经
  std::io::Error 转换时收到垃圾值；与物化机制关联渐远，属 numpy Windows
  深度兼容路径。诊断需跟踪 numpy C 侧 Windows API 调用链，成本高。
- 状态：记录为专职项；numpy 已从 AV 崩溃连闯六关（远超"越过 PyList 墙"
  原始目标），每关修复均已合并主仓。

## 方法论
- 独立 worktree（RustPython-wt-aa）实施，主仓保持稳定；验证后 cherry-pick 回主仓。
- 每关修复均先加针对性调试 eprintln → 重建（4-8 分钟）→ 观察 numpy 下一状态 → 清除调试 → 提交。
- 构建要点：`cargo build --release -p rustpython --features capi -p rustpython-capi`
  必须先产出 `target/release/rustpython_capi.dll`（顶层 build.rs 的
  export_windows_capi_symbols 从该 DLL 拷贝导出到 exe）；若 exe 导出数不足
  （worktree 首建时曾只有 134 个 C shim），touch 顶层 build.rs 强制重跑。
- shim 重建：`powershell -ExecutionPolicy Bypass -File bench\make_python_dll_shims.ps1`
  （新符号需进 python314.dll/rustpythonapi.dll 转发表）。

## 回归确认
- `cargo test -p rustpython-vm --lib`：53 passed / 2 failed（2 个为预先存在的
  str 标题大小写失败），无新增回归。
