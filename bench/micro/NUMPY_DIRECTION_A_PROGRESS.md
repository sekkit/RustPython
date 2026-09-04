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

## 当前关卡（诊断中）
- 现象：`AttributeError: '\x00' object has no attribute '__name__'`
  触发：numpy `_core/dtypes.py:80` `_add_dtype_helper(DType, alias)` 的
  `DType.__name__` 访问。
- 已确认：被访问对象的 `ob_type@8 = 0x7fff0d5f8790`（每次运行相同，堆地址），
  该 ob_type 指向对象的 class.name = "\0"（NUL），对象本身非 foreign、
  ob_type 也非 foreign。`PyType_FromSpec` 未创建名字含 "DType" 的 heap type。
- 推测：numpy 的 DType 类是通过 C 侧其它机制创建的静态类型对象，我们的
  `resolve_type_ptr` fallback 路径（pytype.rs 约 239 行 `(&*(ptr as *mut PyObject))
  .to_owned().downcast::<PyType>()`）将其强转成 PyType 后 name 读出 NUL。
- 状态：交由子代理 e90eda07 诊断（实现问题 vs 架构墙）。

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
