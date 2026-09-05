# numpy 方向 A 边界物化 — 实施进展记录

> 状态：**六关连闯完成**。numpy 从 AV 崩溃推进六关（越过 PyList 布局墙 + 五个
> 后续关卡），每关修复均已合并主仓。第六关（decode_error_kind）判定为需要
> numpy C 侧 Windows 路径专职分析，记录待排期。

## 里程碑汇总（主仓 commits）

| 关卡 | 现象 | 修复 | commit |
|---|---|---|---|
| 0 → 1 | PyList_SET_ITEM 写 ob_item@0x18 AV | PyList_New 返回 CPython 布局缓冲 + 物化 + 双路径 | b559bdbc8 |
| 1 → 2 | TypeError: unhashable type: '' | 空 hash slot = 身份哈希 | 8e6e27e30 |
| 2 → 3 | getattro unwrap panic | 回退 object.__getattribute__ | 0aa793e3e |
| 3 → 4 | AttributeError: '\x00' __name__ | materialize_cpy_type 物化回调 | 6ad759994 |
| 4 → 5 | load_attr AV（specialize 处） | specialize_load_attr 探测物化防护 | 92d761431 |
| 5 → 6 | std::sys::io::error::windows::decode_error_kind AV | 待专职 | — |

## 第五关（92d761431）细节
- 现象：`_multiarray_umath` init 注册首个 DType 后，
  `ExecutingFrame::load_attr` NATIVE-CRASH。
- 根因：`specialize_load_attr` 直接 `obj.class()`——对裸 CPython 布局对象
  （numpy 经 C 参数打包直接进帧、未注册 foreign）读取 CPython 头部字节当
  RustPython PyType，崩溃。`get_attr_inner` 已有物化处理但 specialize 抢先。
- 修复：specialize 开头探测物化回调（内部对非 type 对象安全返回 NULL）——
  is_foreign 或物化命中即跳过特化、回退 slow 路径。
- 关键工具：RUSTPYTHON_NP_DBG 门控的 load_attr 探针（打印栈顶原始指针，
  在 pop_value 之前/之后双层观察），定位崩溃对象 ob_type 指向 numpy DLL。

## 第六关（待专职）细节
- 现象：`std::sys::io::error::windows::decode_error_kind` (read) 0x10 AV，
  rdx=垃圾值被当 Windows os error 码。
- 性质：numpy 某 Windows API 路径（console/io/编码检测）产生的错误状态经
  std::io::Error 转换时收到垃圾值。诊断需跟踪 numpy C 侧 Windows API 调用链
  （numpy 无源码、仅 .pyd），成本高。
- 判定：记录为 numpy Windows 深度兼容专职项。numpy 已六关连闯，远超
  "越过 PyList 墙"原始目标。
- 建议：若专职推进，先定位 decode_error_kind 的调用源（crash_diag 回溯
  显示 fn=''——无 Python 帧信息，需给 crash_diag 补当前 Python 帧
  file:line 定位原生 AV 源码位置）。

## 关键发现（方法论）
- exe 启动走 `init_main_interpreter`（src/lib.rs:136）而非 `Py_InitializeEx`，
  foreign_dispatch::set() 此前从未在二进制安装（is_foreign 恒 false、
  wrap_foreign 恒 None）——前三个 commit 的 foreign 机制在 exe 中部分休眠；
  6ad759994 在 init_main_interpreter 补装整个 foreign 调度表 + 物化回调。
- numpy 2.5.2 内建 DType/标量类是编译进 _multiarray_umath.pyd 的静态
  CPython PyTypeObject 结构体（非 PyType_FromSpec），tp_name@24 可读、
  vtable=0、ob_type 指向 DLL 内静态 metaclass PyArray_DTypeMeta_Type。
- 每关修复流程：针对性调试 eprintln（RUSTPYTHON_NP_DBG 门控）→ 重建
  （4-9 分钟）→ 观察 numpy 下一状态 → 清除调试 → 提交 → cherry-pick 主仓。

## 回归确认
- `cargo test -p rustpython-vm --lib`：53 passed / 2 failed（2 个为预先存在的
  str 标题大小写失败），无新增回归。
- 主仓 exe 重建后 numpy 各关状态可复现（需 touch build.rs 强制重跑导出）。