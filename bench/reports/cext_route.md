# RustPython C 扩展路线调查报告(cext_route)

- 调查对象:`C:\Dev2\luna-lang\RustPython`(Rust 编写的 Python 3.14 解释器)
- 调查时间:2026-08(基于仓库 `heads/main-dirty:529ba89de` 的 release 构建)
- 运行方式:`$env:RUSTPYTHONPATH = "C:\Dev2\luna-lang\RustPython\Lib"; .\target\release\rustpython.exe <脚本>`
- 约束:只读调查,未修改任何仓库源文件;所有探针脚本均为新增文件(位于 `bench/labs/` 下)
- 对照基线:本机 CPython 3.11.9(用于区分"RustPython 缺陷"与"ctypes 标准行为")

---

## 0. 结论速览

| 主题 | 结论 |
|---|---|
| ctypes 基础(struct/union/指针/数组/字符串/errno) | **大部分可用**,13 项关键功能 9 项通过 |
| 真正的 ctypes 缺陷 | **4 个**:位域读写、结构体数组元素访问、回调作为函数参数、`cast()` 语义(回调地址/`byref` 两处) |
| Win32 深层调用(结构体指针参数) | **全部通过**(GetSystemInfo / GetVersionExW / QueryPerformanceCounter) |
| Win32 回调(EnumWindows) | 底层回调机制**可用**(291 个窗口枚举成功),但标准写法 `EnumWindows(Proc(cb), 0)` 因参数转换缺陷失败 |
| capi crate | 325 个导出函数、无 README、pyo3 abi3 后端定位;缺 `PyArg_ParseTuple`/`PyType_FromSpec`/`PyModule_Create` 等模块初始化核心;默认构建未链接 |
| numpy 失败根因 | **三层叠加**:① pip 标签不匹配无 wheel 可装;② sdist 构建缺 `cProfile`;③ 导入器不认 `.pyd`(`_imp.extension_suffixes()` 返回空)→ **C 扩展 ABI 支持完全缺失**,纯 Python 部分本身可导入 |

---

## 1. ctypes 功能深度测试

探针脚本:`bench/labs/ctypes_probe/probe1_struct_union.py`、`probe2_pointers.py`、`probe3_callbacks.py`、`probe4_misc.py`、`diag1.py`(细节诊断)。
所有"FAIL"均与 CPython 3.11.9 实测对比,标注是否为 RustPython 独有缺陷。

### 1.1 功能矩阵

| 功能 | 结果 | 错误信息 / 说明 |
|---|---|---|
| 简单 struct + `sizeof`/字段 `offset` | ✅ OK | `Point(x,y)` sizeof=8,offset 正确 |
| 嵌套 struct | ✅ OK | 内层字段赋值/读取正确 |
| struct 内数组字段 | ✅ OK | `c_int * 3` 字段正确 |
| `from_buffer_copy` | ✅ OK | 从字节构造结构体 |
| **位域(bitfield)** | ❌ **FAIL(真缺陷)** | `f.a=5; f.b=3` 后 `f.a` 读回 **3**——字段互相覆盖(CPython 读回 5)。`sizeof`=4 正确,但读写不做位掩码/移位 |
| union | ✅ OK | 大小 4,字段互写正确 |
| 数组创建/初始化 | ✅ OK | `(c_int*5)(1,2,3,4,5)` |
| **结构体数组元素访问** | ❌ **FAIL(真缺陷)** | `(Point*3)()[1]` 返回 **`int`(0)** 而非 `Point` 副本(CPython 返回 Point);`pts[1].x = 7` 报 `AttributeError: 'int' object has no attribute 'x'` |
| 对齐/offset | ✅ OK | `c_char` + `c_int` 结构 sizeof=8,`i.offset==4` |
| `create_string_buffer` 往返 | ✅ OK | `value`/`raw` 正确 |
| struct 内指针字段 | ✅ OK | `POINTER(c_int)` 字段赋值/取值 |
| 指针 `pointer()`/`contents` 读 | ✅ OK | `p.contents.value` 正确 |
| `contents`/`p[0]` 写回普通 `c_int` | ⚠️ 与 CPython 一致 | CPython 对普通 `c_int` 同样不写回原对象(内联存储怪癖);**通过 struct/数组写回正常**(见下) |
| 指针写回(struct/数组) | ✅ OK | `ptr.contents.x = 99` → `p.x==99`;`pa[1]=42` → `arr[1]==42`(与 CPython 一致) |
| 指针算术 `ptr + int` | ⚠️ 与 CPython 一致 | CPython 3.11 同样抛 `TypeError: unsupported operand type(s) for +`;指针索引 `p[0]`、`cast(addrof(arr)+n, POINTER(...))` 均正常 |
| `cast(pointer(x), ...)` | ✅ OK | 指针类型互转、转 `c_void_p` 均正确 |
| **`cast(byref(x), ...)`** | ❌ **FAIL(真缺陷,轻微)** | `TypeError: cast() argument 1 must be a ctypes instance, not CArgObject`;CPython 允许(返回 5) |
| `byref`/`addressof` | ✅ OK | 正常 |
| `c_void_p` 往返 | ✅ OK | `cast` → `value == addressof` → 转回读值 |
| `c_char_p` 往返 | ✅ OK | 赋值/读取正确 |
| `string_at` / `wstring_at` | ✅ OK | 正确截取 |
| NULL 指针 `contents` | ⚠️ 与 CPython 一致 | `ValueError: NULL pointer access`(CPython 相同) |
| `CFUNCTYPE` 直接调用 | ✅ OK | `f(21)==42`;闭包状态、多参数、`c_void_p` 参数均正常 |
| **回调地址 `cast(f, c_void_p)`** | ❌ **FAIL(真缺陷)** | 返回的是 **buffer 地址**(RW 数据页)而非函数指针;直接当函数指针调用会触发 **0xC0000005 访问违例**(DEP)。真实 code_ptr 在 buffer 首指针字内,保护属性为 `EXECUTE_READWRITE`(正常) |
| `WINFUNCTYPE` | ✅ OK | 类型存在可用 |
| 回调类级 `argtypes`/`restype` 内省 | ⚠️ 与 CPython 一致 | 返回 attribute descriptor(CPython 3.11 相同) |
| 回调实例调用 + `argtypes` | ✅ OK | `f(3)==9`;实例 `argtypes` 返回 tuple(CPython 返回 list,微小偏差,无实际影响) |
| `ctypes.wintypes` | ✅ OK | 仅缺 `LRESULT`——**CPython 3.11 同样没有 `LRESULT`**,非缺陷;其余(HWND/HANDLE/DWORD/MSG/RECT…)齐全 |
| `use_errno` + `get/set_errno` | ✅ OK | 正常 |
| `use_last_error` + `get/set_last_error` | ✅ OK | 正常 |
| 动态 `_fields_` | ✅ OK | 事后赋值 `_fields_` 可用 |
| 结构体继承 | ✅ OK | sizeof 正确(8) |
| `memmove` / `memset` | ✅ OK | 正确 |
| `HRESULT` | ⚠️ 与 CPython 一致 | `HRESULT(0x80004005).value == -2147467259`(CPython 相同) |
| 指针参数传 `None` | ✅ OK | `GetModuleHandleW(None)` 正常 |
| `ctypes.pythonapi` | ⚠️ 桩 | 存在但为 `_PyApiStub`(`Lib/ctypes/__init__.py:561-587`),调用抛 `NotImplementedError`;无原生 python DLL 可解析 |

### 1.2 真缺陷清单(4 项,含定位)

| # | 缺陷 | 表现 | 代码定位(文件:行) |
|---|---|---|---|
| B1 | 位域读写不做掩码/移位 | `a=5,b=3` 后 `a` 读回 3 | `crates/vm/src/stdlib/_ctypes/base.rs`:`PyCData::set_field`(873)/`get_field`(1015)忽略 `bit_offset_val`/`bitfield_size` |
| B2 | 结构体数组元素返回 `int` | `(Point*3)()[i]` 返回 int,无法 `pts[i].x=7` | `crates/vm/src/stdlib/_ctypes/array.rs`:`read_element_from_buffer`(583)经 `crates/host_env/src/ctypes.rs`:`read_array_element`(757)按原始字节解码为整数,无"构造元素类型实例"分支 |
| B3 | 回调无法作为 DLL 函数参数 | `EnumWindows(Proc(cb), 0)` → `TypeError: Unsupported argument type`(设 argtypes)/`Don't know how to convert parameter`(不设) | `crates/vm/src/stdlib/_ctypes/function.rs`:`conv_param`(125)与 `ArgumentType::convert_object`(206)均无 `PyCFuncPtr` 分支;`PyCFuncPtr::get_func_ptr`(631)已能取地址 |
| B4 | `cast()` 语义错误 | ① 回调对象 cast 返回 buffer 地址而非函数指针(导致 DEP 崩溃);② `cast(byref(x))` 直接 TypeError(CPython 允许) | `crates/vm/src/stdlib/_ctypes/function.rs`:`cast_impl`(532)对 `PyCFuncPtr` 落入 `PyCData` 分支返回 `buffer.as_ptr()`;且无 `CArgObject` 分支 |

---

## 2. 纯 ctypes 库 / Win32 深层调用测试

探针:`bench/labs/ctypes_probe/win32_probe.py`(手写 Win32 调用,即任务所述"手写 ctypes 调 user32/GetSystemMetrics 脚本"路线)、`win32_enumwindows_direct.py`。

### 2.1 结果

| 测试 | 结果 | 说明 |
|---|---|---|
| `user32.GetSystemMetrics(SM_CXSCREEN/SM_CYSCREEN)` | ✅ OK | 返回 4096×1728 |
| `kernel32.GetSystemInfo`(输出结构体指针) | ✅ OK | pageSize=4096,procs=64,arch=9 |
| `kernel32.GetVersionExW`(输入/输出结构体 + `WCHAR[128]` 数组) | ✅ OK | 6.2 build 9200 |
| `user32.EnumWindows`(WINFUNCTYPE 回调,标准写法) | ❌ FAIL | `TypeError: Unsupported argument type`(缺陷 B3) |
| `user32.EnumWindows` + `GetWindowTextW` | ❌ FAIL | 同上(卡在 EnumWindows 参数转换,`GetWindowTextW` 本身未测到) |
| `GetSystemInfo` 的 `c_void_p` 字段往返 | ✅ OK | minAddr=0x10000,maxAddr=0x7ffffffeffff |
| `GetCurrentProcessId` / `GetTickCount64`(多参数、无参) | ✅ OK | pid/tick 正常 |
| `QueryPerformanceCounter`(union 输出参数) | ✅ OK | QPC 正常 |
| **`EnumWindows` + 真实 code_ptr(绕过转换缺陷)** | ✅ **OK** | 枚举 **291 个窗口**,回调从原生代码正常进入 Python,`err=0`——证明 libffi 回调底层机制可用 |

### 2.2 关键实验:回调"崩溃"真相

- 首次用 `ctypes.cast(f, c_void_p).value` 拿地址传给 `EnumWindows` → **0xC0000005 崩溃**(两次复现)。
- `VirtualQuery` 检查:`cast` 返回的地址所在页保护属性为 **PAGE_READWRITE(0x4,不可执行)** → DEP 违例;而 buffer 内存储的真实 code_ptr 页保护为 **PAGE_EXECUTE_READWRITE(0x40)**(libffi closure 分配正常)。
- 用真实 code_ptr 调用 `EnumWindows` → 完美工作。
- 结论:**回调基础设施(libffi `CallbackThunk` + `thunk_callback`)是好的**,崩溃只是缺陷 B4①(cast 返回错误地址)的次生现象。

---

## 3. capi 现状调查

### 3.1 基本信息

- **无 README**:`crates/capi/` 下没有任何 `.md` 文件。唯一官方描述是 `Cargo.toml` 的 `description = "Minimal CPython C-API compatibility exports for RustPython"`。
- **构建形态**:`crate-type = ["cdylib", "rlib"]`——理论上可编译为共享库(python3.dll 的替代品),但:
  - 主 crate 的 `capi` feature **不在 `default` features 中**(`Cargo.toml`:`default = ["threading", "stdlib", "stdio", "importlib", "ssl-rustls-aws-lc", "host_env", "sqlite"]`);
  - 实测 release 版 `rustpython.exe` **不导出** `PyObject_GetAttr`(GetProcAddress 失败)——即默认构建未链接 capi。
- **定位**:`pyo3-rustpython.config`(`implementation=RustPython, version=3.15, shared=true, target_abi=RustPython-abi3t-3.15, suppress_build_script_link_lines=true`)——它是 **pyo3 的 abi3 后端**:让 pyo3 编译的扩展链接到 RustPython 的 C API 导出。capi 自带测试(`#[cfg(test)] mod tests`)直接用 `pyo3::ffi` 验证(如 `PyCFunction_New` 测试)。
- **近期活跃**:git log 显示 unicode/dict/eval/descriptor/codecs/async-iterator/refcount 等持续增量提交。

### 3.2 导出规模与分布

共 **325 个** `pub (unsafe) extern "C" fn` 导出,按文件:

| 文件 | 数量 | 文件 | 数量 |
|---|---|---|---|
| unicodeobject.rs | 40 | object.rs | 38 |
| longobject.rs | 30 | dictobject.rs | 22 |
| codecs.rs | 19 | pyerrors.rs | 19 |
| abstract_.rs | 16 | ceval.rs | 12 |
| objimpl.rs | 12 | pylifecycle.rs | 11 |
| listobject.rs | 11 | pycapsule.rs | 8 |
| pymem.rs | 8 | setobject.rs | 8 |
| descrobject.rs | 6 | floatobject.rs | 6 |
| tupleobject.rs | 6 | bytearrayobject.rs | 5 |
| critical_section.rs | 4 | refcount.rs | 4 |
| boolobject/bytesobject/complexobject/import/methodobject/moduleobject/weakrefobject | 各 3 | pyframe/pystrcmp/warnings | 各 2 |
| genericaliasobject/memoryobject/osmodule/traceback | 各 1 | | |

### 3.3 已实现的核心函数(抽查)

- **对象**:`PyObject_GetAttr(WithError)`/`GetAttrString`/`SetAttr`/`GetItem`/`SetItem`/`HasAttr`/`Repr`/`Str`/`Type`/`RichCompare(Bool)`/`IsTrue`/`GenericGetAttr`/`Vectorcall`
- **数值**:`PyLong_FromLong(LongLong)`/`AsLong`/`AsLongLong`、`PyFloat_AsDouble`、`PyComplex_*`(complexobject.rs)
- **序列/映射**:`PyList_New`/`GetItemRef`、`PyTuple_New`/`GetItem`/`SetItem`、`PyDict_New`/`GetItem(Ref/String/WithError)`/`SetItem`/`SetItemString`
- **Unicode**:`PyUnicode_FromString(AndSize)`/`AsUTF8String`/`AsUTF8AndSize`/`FromWideChar` 等 40 个
- **异常**:`PyErr_SetString`/`Occurred` 等 19 个
- **方法定义层(亮点)**:`PyMethodDef` + `PyCFunction_New`/`NewEx`/`PyCMethod_New`,支持 `METH_NOARGS/VARARGS/KEYWORDS/FASTCALL/O`,`PyMethodFlags` 与 RustPython `HeapMethodDef` 对接(`crates/capi/src/methodobject.rs`)
- **解释器/生命周期**:`Py_Initialize(Ex)`、`PyGILState_Ensure/Release`、`PyEval_SaveThread/RestoreThread`、`PyEval_EvalCode/EvalFrame(Ex)`、`PyImport_Import`/`AddModuleRef`/`ExecCodeModuleEx`
- **引用计数**:`_Py_IncRef/_Py_DecRef/Py_NewRef/Py_REFCNT`

### 3.4 关键缺失(C 扩展模块初始化必需,全部缺席)

| 类别 | 缺失函数 |
|---|---|
| 参数解析 | **`PyArg_ParseTuple`、`PyArg_ParseTupleAndKeywords`**(扩展模块 80% 的入口) |
| 返回值构建 | **`Py_BuildValue`** |
| 类型定义 | **`PyType_FromSpec`、`PyType_FromSpecWithBases`** |
| 模块初始化 | **`PyModule_Create`、`PyModuleDef_Init`、`PyModule_AddObject`、`PyModule_AddIntConstant`**(moduleobject.rs 只有 Check/GetName/GetFilename/NewObject) |
| 执行 | `PyRun_SimpleString/SimpleFile` |
| 导入 | `PyImport_ImportModule`(只有 `PyImport_Import`) |
| 缓冲协议 | **`PyObject_GetBuffer`、`PyBuffer_Release`**(数值/数组库必需) |
| 迭代/序列/数字协议 | `PyObject_GetIter`、`PyIter_Next`、`PySequence_*`、`PyMapping_*`、`PyNumber_*` |
| 其他 | `PyObject_CallMethod`、`PyUnicode_AsUTF8`(仅 `AsUTF8AndSize`)、`PyList_GetItem`(仅 `GetItemRef`)、`PyObject_GC_New` |

---

## 4. numpy 失败根因(三层叠加)

### 4.1 实验过程

1. `rustpython -m pip install --target bench\labs\sitepkg numpy` → **失败**:`meson-python: error: ModuleNotFoundError: No module named 'cProfile'`(走 sdist 构建,构建后端缺 `cProfile`)。
2. `pip install --only-binary=:all: numpy` → **失败**:`ERROR: Could not find a version that satisfies the requirement numpy (from versions: none)` —— **没有任何 wheel 匹配**。
   - 原因:RP 计算出的兼容标签为 `rustpython314-cp314t-win_amd64`、`rustpython314-none-win_amd64`、`py314-none-win_amd64`…且 **不含任何 abi3 标签**;numpy 2.5.2 的 `cp314-cp314-win_amd64.whl` 无法匹配。
3. 用宿主机 CPython 强制下载 `numpy-2.5.2-cp314-cp314-win_amd64.whl`(12.6MB)解压到 `bench/labs/sitepkg/numpy251/`,直接 `import numpy`:
   - 第一处失败:**纯 stdlib 缺口**——`os.add_dll_directory` → `nt._add_dll_directory` 缺失(`AttributeError`),numpy 的 delvewheel 引导代码在第一行就挂。
   - 在探针脚本里 stub `nt._add_dll_directory` 并加 `numpy.libs` 到 PATH 后:**纯 Python 部分可正常导入**(`numpy/__init__.py`、`_core/__init__.py`、`__config__.py` 全部执行到扩展导入)。
   - 最终失败:`from . import _multiarray_umath` → **`ModuleNotFoundError: No module named 'numpy._core._multiarray_umath'`** —— `.pyd` 文件就在磁盘上(21 个编译模块),但导入器根本不去找它。

### 4.2 根因定位(代码级)

- **导入器不认扩展后缀**:`crates/vm/src/stdlib/_imp.rs:197-200` —— `_imp.extension_suffixes()` 返回 **`Vec::new()`**(空列表);实测 `importlib.machinery.EXTENSION_SUFFIXES == []`。CPython 在 Windows 返回 `['.pyd']`。
- **虚假的 EXT_SUFFIX**:`crates/vm/src/stdlib/_sysconfig.rs:11-16` 把 `EXT_SUFFIX` 硬编码为 `".pyd"`(代码自注 `FIXME: This is an entirely wrong implementation of EXT_SUFFIX`),`SOABI = None`。
- **无加载机制**:即使找到 `.pyd`,也没有 LoadLibrary + `PyInit_*` 入口的加载路径;capi 未链接进默认构建(§3.1),`ctypes.pythonapi` 是桩(§1.1)。

### 4.3 结论

- **"无 C 扩展 ABI 支持"是主因**:导入器层面完全不存在 `.pyd` 加载(extension_suffixes 为空),这与 capi 未接入、pythonapi 为桩是一致的。
- **纯 Python 部分本身没有失败**:在补齐 `nt._add_dll_directory` 后,`numpy` 的纯 Python 引导代码可以一路跑通到扩展导入点。
- pip 层(numpy 装不上)与 import 层(装上也不能 import)是**两个独立问题**:前者是标签/abi 计算问题,后者是扩展加载缺失。

---

## 5. C 扩展路线建议(分阶段)

### 阶段 A:ctypes 增强(纯 Rust,不依赖 capi,可立即做,收益最大)

| 优先级 | 事项 | 具体位置 |
|---|---|---|
| A1 | **修复位域读写**:get/set 时按 `bit_offset`/`bit_size` 做掩码+移位(参考 CPython `_ctypes/cfield.c` 的 `b_get`/`b_set`) | `crates/vm/src/stdlib/_ctypes/base.rs`:`set_field`(873)、`get_field`(1015);字段元数据在 `PyCField::new_bitfield`(1283) |
| A2 | **结构体数组元素访问**:元素类型为 Structure/Union 时,从 buffer 切片构造元素类型实例(返回副本,写入时写回),而不是按原始字节解码成 int | `crates/vm/src/stdlib/_ctypes/array.rs`:`getitem_by_index`(549)、`read_element_from_buffer`(583)、`setitem_by_index`(786);`crates/host_env/src/ctypes.rs`:`read_array_element`(757) |
| A3 | **回调作为函数参数**:`conv_param` 与 `ArgumentType::convert_object` 增加 `PyCFuncPtr` 分支,取 `get_func_ptr()` 作为 `CArgValue::pointer`(此时 `EnumWindows(Proc(cb), 0)` 标准写法即可用) | `crates/vm/src/stdlib/_ctypes/function.rs`:`conv_param`(125)、`convert_object`(206)、`PyCFuncPtr::get_func_ptr`(631) |
| A4 | **修复 `cast()`**:① `PyCFuncPtr` 返回 buffer 内存储的函数指针(参考 `PyCSimple` 分支,562-565),而非 buffer 地址;② 增加 `CArgObject`(byref)分支以匹配 CPython | `crates/vm/src/stdlib/_ctypes/function.rs`:`cast_impl`(532) |
| A5 | **补齐 stdlib 小缺口**(影响纯 Python 包安装/导入):`nt._add_dll_directory`(delvewheel 类包)、`cProfile`(meson-python 等构建后端) | `crates/vm/src/stdlib/nt.rs`(确认缺失位置);`Lib/cProfile.py` 从 CPython 同步 |

> 完成 A1–A4 后,`wmi` 类纯 ctypes 包、pywin32 的纯 ctypes 部分、手写 Win32 自动化脚本即可解锁;`EnumWindows`/`SetWindowsHookEx` 类回调 API 也可用。

### 阶段 B:capi 优先级(解锁 pyo3 系扩展)

| 优先级 | 事项 | 具体位置 |
|---|---|---|
| B1 | **把 capi 接入默认构建**:主 crate `default` features 加入 `capi`;将 325 个导出符号暴露给本进程(exe/dll),使 `ctypes.pythonapi` 可解析(把 `Lib/ctypes/__init__.py:561-587` 的 `_PyApi` 桩改为 `PyDLL(None)` 或加载导出 DLL) | `Cargo.toml`(features)、`Lib/ctypes/__init__.py` |
| B2 | **模块初始化四件套**(任何 C 扩展 import 的第一步):`PyModule_Create`、`PyModuleDef_Init`、`PyModule_AddObject`/`AddIntConstant`;`PyType_FromSpec(WithBases)` | `crates/capi/src/moduleobject.rs`(现仅 3 函数)、`crates/capi/src/object.rs`(新增 `PyType_FromSpec` 需要对接 `rustpython_vm` 的 `PyType`/`HeapType` 创建) |
| B3 | **参数解析与返回值**:`PyArg_ParseTuple(AndKeywords)`(对接 `FuncArgs`/`PyMethodFlags` 解析,参考 `crates/capi/src/methodobject.rs` 现有 `call_function` 系列)、`Py_BuildValue` | `crates/capi/src/methodobject.rs`、新增 `argparse` 模块 |
| B4 | **扩展加载机制**:`_imp.extension_suffixes()` 返回平台真实后缀(Windows:`['.pyd']`);新增 `.pyd` 的 `_find_and_load` 路径(LoadLibrary → 找 `PyInit_<modname>` → 调 `PyModule_Create`) | `crates/vm/src/stdlib/_imp.rs:197-200`、`crates/vm/src/import.rs`(当前走 `vm.importlib._find_and_load`) |
| B5 | **缓冲协议**:`PyObject_GetBuffer`/`PyBuffer_Release`(numpy 类数值库的硬前提),对接 VM 的 `PyBuffer` | `crates/capi/src/memoryobject.rs`(现仅 1 函数) |

### 阶段 C:生态解锁次序

1. **立即可解锁(阶段 A 完成后)**:纯 ctypes 生态——Win32 自动化/系统管理脚本、`wmi`(纯 ctypes,需 win32com?——注意 `wmi` 实际依赖 pywin32 的 win32com,属 C 扩展;更准确的是"手写 ctypes"与 `pywin32` 中纯 ctypes 的部分)、`psutil` 的纯 Python 功能回退、ctypes 包装的系统 API。
2. **短期(阶段 B1–B3 后)**:pyo3 系扩展(rust 生态:rust-numpy、rust-cpython、pyo3 构建的现代扩展)——capi 测试已证明 pyo3 扩展能在该 ABI 上运行,缺的只是上述初始化函数。
3. **长期(B4–B5 + 大量 ABI 补齐后)**:传统 CPython C 扩展(numpy/pandas/lxml/cryptography)。前提链:扩展加载(`PyInit_*`)→ 模块/类型/参数解析 → 缓冲协议与数值语义。在这之前,numpy 一类 **无法通过任何途径** 在 RP 中运行(当前连 `.pyd` 都不被导入器识别)。

---

## 6. 附:探针与产物清单

- `bench/labs/ctypes_probe/probe1_struct_union.py` ~ `probe4_misc.py`:ctypes 功能矩阵探针
- `bench/labs/ctypes_probe/diag1.py`、`diag2_callbacks.py`、`diag3_crash.py`:缺陷细节/崩溃隔离诊断
- `bench/labs/ctypes_probe/win32_probe.py`、`win32_enumwindows_direct.py`:Win32 深层调用与回调验证
- `bench/labs/sitepkg/numpy_probe.py`:numpy import 探针(含 `nt._add_dll_directory` stub)
- `bench/labs/sitepkg/wheels/numpy-2.5.2-cp314-cp314-win_amd64.whl`、`bench/labs/sitepkg/numpy251/`:强制下载并解压的 numpy wheel(实验用)
- 全部探针输出均可在仓库内复跑:`$env:RUSTPYTHONPATH = "...\Lib"; .\target\release\rustpython.exe bench\labs\ctypes_probe\<script>`
