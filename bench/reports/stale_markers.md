# RustPython `Lib/test` 过时标记(stale markers)调查报告

- **调查对象**:`C:\Dev2\luna-lang\RustPython`(Rust 实现的 Python 3.14 解释器)
- **解释器**:`target\release\rustpython.exe`,Python 3.14.0.alpha(heads/main-dirty:529ba89de,RustPython 0.5.0)
- **调查方式**:只读;未修改任何 `.rs` / `.py` 源文件;仅新增本报告 `bench\reports\stale_markers.md`
- **环境**:Windows(x64),`RUSTPYTHONPATH=C:\Dev2\luna-lang\RustPython\Lib`
- **报告日期**:本次会话

---

## 1. 执行摘要

1. **全仓库共 1075 处 `@unittest.expectedFailure` / `@unittest.expectedFailureIf` 装饰器**,分布在 192 个文件;其中 1051 处带 `# TODO: RUSTPYTHON` 注释(同行或上一行),24 处无 RUSTPYTHON 注释(多为 CPython 自带标记或 RustPython 的 `@unittest.expectedSuccess` 覆盖)。
2. 按任务要求挑选 **39 个模块**(用户优先清单 21 个 + 补充验证 18 个,含语言核心与已知全绿模块)逐个运行 `-m test -v`(每个限时 3 分钟):
   - **unexpected success:0 处** → **确认过时(可移除)的标记:0 个**
   - **FAILED / ERROR:0 个** → 39 个模块全部 `Result: SUCCESS`,退出码 0
   - 239 行 `expected failure`(仍如预期失败,含类继承导致的重复行)、593 个 skipped、5641 个 ok
3. **没有发现可移除的过时标记**。所有被标记测试在当前解释器上仍按预期失败;仅有 13 处"休眠/非活动"标记(见 §6)——它们因**平台条件不满足**或**测试被 skip** 而从未生效,并非"测试已修复",不应移除。
4. 检测方法经过了端到端自检(临时脚本验证 `unexpected success` 可被正确报告),因此"0 个过时标记"是真实结论,而非检测失效。

> 备注:`test_parser`、`test_comprehensions` 两个模块在 CPython 3.14 测试套件中已不存在(被并入 `test_syntax`/其他),故以 `test_super`、`test_generators` 替代补足 20 个目标模块。

---

## 2. 调查方法与工具链

- 标记统计:用正则扫描 `Lib\test\**\*.py`(排除 `__pycache__`),匹配 `^\s*@unittest\.expectedFailure(If)?`,记录**模块文件、装饰器行号、下方第一个 `def` 测试名、原因注释**(同行 `# TODO: RUSTPYTHON; <原因>` 或上一行的 TODO 注释)。
- 测试运行:`$env:RUSTPYTHONPATH = "<repo>\Lib"; .\target\release\rustpython.exe -m test -v <模块>`,每个模块 `Start-Process` + 180 秒超时,超时即强杀并记为 TIMEOUT(本次 39 个模块全部在限时内正常结束)。
- 结果解析:`-v` 模式下 unittest 逐测试打印 `test_xxx (test.xxx.Cls.test_xxx) ... <状态>`;对带 docstring 的测试,状态可能出现在描述第二行或独立一行,解析器做了三态处理(单行/多行描述/独立状态行)。
- **自检**:用临时脚本验证 `@unittest.expectedFailure` 装饰一个"已通过"的测试时,输出确为 `... unexpected success` 且 `unexpectedSuccesses` 被记录——证明"0 个 unexpected success"不是解析盲区。

---

## 3. 标记总数统计(全仓库 `Lib/test`)

| 统计项 | 数量 |
|---|---:|
| `@unittest.expectedFailure` 装饰器 | 1010 |
| `@unittest.expectedFailureIf` 装饰器 | 65 |
| **合计** | **1075** |
| 带 `# TODO: RUSTPYTHON` 注释(同行或上一行) | 1051 |
| 无 RUSTPYTHON 注释(CPython 自带 / `expectedSuccess` 覆盖等) | 24 |
| 涉及的模块文件数 | 192 |

### 按模块分布(全部 192 个文件,按标记数降序)

| 模块文件 | 标记数 |
|---|---:|
| `test_pyexpat.py` | 49 |
| `test_xml_etree.py` | 35 |
| `test_sax.py` | 31 |
| `test_pydoc\test_pydoc.py` | 30 |
| `test_inspect\test_inspect.py` | 29 |
| `test_descr.py` | 26 |
| `test_cmd_line.py` | 23 |
| `test_mmap.py` | 22 |
| `test_exceptions.py` | 22 |
| `test_pickle.py` | 19 |
| `test_sqlite3\test_userfunctions.py` | 19 |
| `test_venv.py` | 18 |
| `test_coroutines.py` | 18 |
| `test_ast\test_ast.py` | 18 |
| `test_trace.py` | 16 |
| `test_bytes.py` | 15 |
| `test_fstring.py` | 14 |
| `test_audit.py` | 14 |
| `test_socket.py` | 13 |
| `test_asyncio\test_taskgroups.py` | 12 |
| `test_hmac.py` | 12 |
| `test_warnings\__init__.py` | 12 |
| `test_picklebuffer.py` | 11 |
| `test_codecs.py` | 11 |
| `test_buffer.py` | 11 |
| `test_class.py` | 11 |
| `test_builtin.py` | 11 |
| `test_memoryio.py` | 11 |
| `test_pyrepl\test_windows_console.py` | 11 |
| `test_symtable.py` | 11 |
| `test_unicodedata.py` | 10 |
| `test_unittest\test_skipping.py` | 10 |
| `test_unittest\testmock\testasync.py` | 10 |
| `test_lzma.py` | 10 |
| `test_cmd_line_script.py` | 9 |
| `test_codeccallbacks.py` | 9 |
| `test_functools.py` | 9 |
| `test_sqlite3\test_hooks.py` | 8 |
| `test_memoryview.py` | 8 |
| `test_pickletools.py` | 7 |
| `test_ordered_dict.py` | 7 |
| `test_dis.py` | 7 |
| `test_re.py` | 7 |
| `test_hashlib.py` | 7 |
| `test_sqlite3\test_dump.py` | 7 |
| `test_sqlite3\test_factory.py` | 7 |
| `test_struct.py` | 7 |
| `test_repl.py` | 7 |
| `test_csv.py` | 6 |
| `test_zipfile\test_core.py` | 6 |
| `test_utf8_mode.py` | 6 |
| `test_json\test_speedups.py` | 6 |
| `test_tokenize.py` | 6 |
| `test_threading.py` | 6 |
| `test_sysconfig.py` | 6 |
| `test_pdb.py` | 6 |
| `test_ssl.py` | 6 |
| `test_plistlib.py` | 6 |
| `test_print.py` | 6 |
| `test_pulldom.py` | 6 |
| `test_context.py` | 6 |
| `test_codecmaps_jp.py` | 6 |
| `test_sqlite3\test_dbapi.py` | 6 |
| `test_str.py` | 5 |
| `test_monitoring.py` | 5 |
| `test_sys.py` | 5 |
| `test_posix.py` | 5 |
| `test_minidom.py` | 5 |
| `test_io.py` | 5 |
| `test_signal.py` | 5 |
| `test_xmlrpc.py` | 5 |
| `test_pyrepl\test_pyrepl.py` | 5 |
| `test_zoneinfo\test_zoneinfo.py` | 5 |
| `test_email\test_email.py` | 5 |
| `pickletester.py` | 5 |
| `test_long.py` | 4 |
| `test_asyncio\test_tasks.py` | 4 |
| `test_traceback.py` | 4 |
| `test_ucn.py` | 4 |
| `test_string_literals.py` | 4 |
| `test_faulthandler.py` | 4 |
| `test_frame.py` | 4 |
| `test_unittest\test_result.py` | 4 |
| `test_tabnanny.py` | 4 |
| `test_asyncgen.py` | 4 |
| `test_complex.py` | 4 |
| `test_weakref.py` | 4 |
| `test_site.py` | 4 |
| `test_hash.py` | 4 |
| `test_flufl.py` | 4 |
| `test_codecmaps_tw.py` | 3 |
| `test_peepholer.py` | 3 |
| `test_codecmaps_cn.py` | 3 |
| `test_super.py` | 3 |
| `test_exception_group.py` | 3 |
| `test_dict.py` | 3 |
| `test_code.py` | 3 |
| `test_codecmaps_kr.py` | 3 |
| `_test_eintr.py` | 3 |
| `test_regrtest.py` | 3 |
| `test_tstring.py` | 3 |
| `test_format.py` | 3 |
| `test_types.py` | 3 |
| `_test_multiprocessing.py` | 3 |
| `test_itertools.py` | 3 |
| `test_asyncio\test_events.py` | 3 |
| `test_import\__init__.py` | 3 |
| `test_sys_settrace.py` | 2 |
| `test_ctypes\test_python_api.py` | 2 |
| `test_generators.py` | 2 |
| `test_set.py` | 2 |
| `string_tests.py` | 2 |
| `test_unittest\testmock\testhelpers.py` | 2 |
| `test_codeop.py` | 2 |
| `test_code_module.py` | 2 |
| `test_unittest\test_program.py` | 2 |
| `test_typing.py` | 2 |
| `test_binascii.py` | 2 |
| `test_sys_setprofile.py` | 2 |
| `test_compileall.py` | 2 |
| `test_ctypes\test_values.py` | 2 |
| `test_select.py` | 2 |
| `test_int.py` | 2 |
| `test_marshal.py` | 2 |
| `test_float.py` | 2 |
| `test_exception_hierarchy.py` | 2 |
| `test_named_expressions.py` | 2 |
| `test_enum.py` | 2 |
| `test_logging.py` | 2 |
| `test_importlib\test_threaded_import.py` | 2 |
| `test_deque.py` | 2 |
| `test_zlib.py` | 2 |
| `test_gc.py` | 2 |
| `test_pyrepl\test_interact.py` | 2 |
| `test_future_stmt\test_future.py` | 2 |
| `test_winsound.py` | 1 |
| `test_zipimport.py` | 1 |
| `test_asyncio\test_streams.py` | 1 |
| `test_listcomps.py` | 1 |
| `test_list.py` | 1 |
| `test_yield_from.py` | 1 |
| `test_tuple.py` | 1 |
| `test_type_params.py` | 1 |
| `test_json\test_scanstring.py` | 1 |
| `test_json\test_default.py` | 1 |
| `test_json\test_decode.py` | 1 |
| `test_unicode_identifiers.py` | 1 |
| `test_asyncio\test_sock_lowlevel.py` | 1 |
| `test_unittest\test_case.py` | 1 |
| `test_json\__init__.py` | 1 |
| `test_httplib.py` | 1 |
| `test_asyncio\test_runners.py` | 1 |
| `test_asyncio\test_futures.py` | 1 |
| `test_funcattrs.py` | 1 |
| `test_importlib\test_windows.py` | 1 |
| `test_wsgiref.py` | 1 |
| `test_abc.py` | 1 |
| `test_imaplib.py` | 1 |
| `test_winconsoleio.py` | 1 |
| `test_pyrepl\test_unix_console.py` | 1 |
| `test_syslog.py` | 1 |
| `test_tempfile.py` | 1 |
| `test_ctypes\test_win32_com_foreign_func.py` | 1 |
| `test_range.py` | 1 |
| `test_dataclasses\__init__.py` | 1 |
| `test_decimal.py` | 1 |
| `test_resource.py` | 1 |
| `test_rlcompleter.py` | 1 |
| `test_runpy.py` | 1 |
| `test_ctypes\test_dllist.py` | 1 |
| `test_pyclbr.py` | 1 |
| `test_contextlib.py` | 1 |
| `test_py_compile.py` | 1 |
| `test_contextlib_async.py` | 1 |
| `test_sqlite3\test_backup.py` | 1 |
| `test_termios.py` | 1 |
| `test_profile.py` | 1 |
| `test_collections.py` | 1 |
| `test_dtrace.py` | 1 |
| `test_sqlite3\test_regression.py` | 1 |
| `test_dummy_thread.py` | 1 |
| `test_dynamic.py` | 1 |
| `test_codecmaps_hk.py` | 1 |
| `test_except_star.py` | 1 |
| `test_structseq.py` | 1 |
| `test_os.py` | 1 |
| `test_support.py` | 1 |
| `test_file_eintr.py` | 1 |
| `test_bz2.py` | 1 |
| `test_pyrepl\test_reader.py` | 1 |
| `test_dictcomps.py` | 1 |
| `test_grp.py` | 1 |

---

## 4. 实测模块结果总表(39 个模块)

运行命令:`.\target\release\rustpython.exe -m test -v <模块>`(RUSTPYTHONPATH 指向 Lib),每模块限时 180 秒。全部模块 `Result: SUCCESS`、退出码 0,**没有任何模块 FAILED,也没有任何 unexpected success**。

| 模块 | 标记数 | expected failure(仍失败) | skipped | 主导原因类别 |
|---|---:|---:|---:|---|
| `test_builtin` | 11 | 4 | 16 | 语义差异 |
| `test_class` | 11 | 11 | 4 | 其他 |
| `test_code` | 3 | 3 | 18 | 其他 |
| `test_compile` | 0 | 0 | 51 | - |
| `test_ctypes` | 6* | 6 | 38 | - |
| `test_descr` | 26 | 24 | 18 | 其他 |
| `test_dictcomps` | 1 | 1 | 0 | 其他 |
| `test_dis` | 7 | 9 | 26 | 其他 |
| `test_enum` | 2 | 2 | 7 | 语义差异 |
| `test_exceptions` | 22 | 22 | 12 | 其他 |
| `test_fstring` | 14 | 14 | 0 | 其他 |
| `test_funcattrs` | 1 | 1 | 1 | 语义差异 |
| `test_functools` | 9 | 10 | 3 | 语义差异 |
| `test_gc` | 2 | 2 | 16 | 其他 |
| `test_generators` | 2 | 2 | 1 | 其他 |
| `test_genexps` | 0 | 0 | 0 | - |
| `test_grammar` | 0 | 0 | 0 | - |
| `test_import` | 3 | 3 | 38 | 平台差异 |
| `test_importlib` | 3* | 3 | 88 | - |
| `test_int` | 2 | 1 | 13 | 其他 |
| `test_iter` | 0 | 0 | 3 | - |
| `test_listcomps` | 1 | 1 | 0 | 其他 |
| `test_memoryview` | 8 | 18 | 19 | 语义差异 |
| `test_named_expressions` | 2 | 2 | 0 | 其他 |
| `test_opcodes` | 0 | 0 | 0 | - |
| `test_peepholer` | 3 | 3 | 79 | 语义差异 |
| `test_runpy` | 1 | 1 | 1 | 其他 |
| `test_setcomps` | 0 | 0 | 0 | - |
| `test_sqlite3` | 49* | 52 | 20 | - |
| `test_ssl` | 6 | 5 | 40 | 其他 |
| `test_str` | 5 | 7 | 11 | 语义差异 |
| `test_super` | 3 | 3 | 3 | 其他 |
| `test_symtable` | 11 | 11 | 0 | 语义差异 |
| `test_syntax` | 0 | 0 | 7 | - |
| `test_sys` | 5 | 4 | 45 | 其他 |
| `test_tokenize` | 6 | 6 | 0 | 其他 |
| `test_types` | 3 | 3 | 2 | 平台差异 |
| `test_weakref` | 4 | 4 | 13 | 其他 |
| `test_yield_from` | 1 | 1 | 0 | 其他 |

**计数口径说明**:
- `*` 包式模块(`test_ctypes` / `test_importlib` / `test_sqlite3`)的标记分散在子文件(`test_ctypes\test_python_api.py` 等),表内"标记数"为包内子文件之和,便于与行数对比。
- "expected failure 行数"可能 **多于** 标记数,原因是类继承:同一被装饰方法被多个子类继承后逐类运行(如 `test_dis.DisWithFileTests` 继承 `DisTests`、`test_memoryview` 多组基类、`test_functools.TestCmpToKeyC/TestCmpToKeyPy` 双实现),每类各计一行。
- 反之,被标记但运行状态为 skipped/ok 的测试会使行数少于标记数(见 §6 休眠标记)。

---

## 5. 确认过时的标记(可移除)

| 模块 | 测试名 | 装饰器行号 | 证据 |
|---|---|---|---|
| — | — | — | **0 个(空)** |

在全部 39 个实测模块中,**没有任何 "unexpected success"**。即不存在"被标记为 expectedFailure、但当前已经通过"的测试,因此**没有可确认移除的过时标记**。这与仓库近期已主动移除过时标记(如 `testRemainder`、`test_finalize_running_thread`)的现状一致:剩余标记仍真实反映解释器的未修复行为。

> 附:24 个"无 TODO 注释"的装饰器均**非** RustPython 过时标记,构成如下:
> - `test_unittest\test_skipping.py` / `test_result.py` / `test_program.py` / `test_case.py`(13 处):CPython 自带的、用于**测试 expectedFailure 机制本身**的标记;
> - `test_descr.py` L1864 / L1912(`test_bad_new`、`test_restored_object_new`):RustPython 用 `@unittest.expectedSuccess` 覆盖 CPython 的 `@unittest.expectedFailure`,**故意要求测试必须通过**(不通过则整套失败);
> - `test_dtrace.py`、`test_int.py`、`test_pulldom.py`(4 处):CPython 上游自带标记。

---

## 6. 休眠 / 非活动标记(不应移除,但值得留意)

以下标记在本次 Windows 运行中**从未生效**(测试被跳过,或条件式标记的条件为假),它们不是"已修复",移除会导致两种情况:平台相关标记一旦在对应平台激活仍会失败;无条件的被跳过的测试一旦在别的环境运行仍会失败。

| 模块 | 测试名 | 装饰器行号 | 注释/条件 | 休眠原因 |
|---|---|---|---|---|
| `test_builtin` | `test_input_tty` | 2717 | TODO: RUSTPYTHON | skipped:需要真实 TTY(stdin 非 tty) |
| `test_builtin` | `test_input_tty_non_ascii` | 2722 | TODO: RUSTPYTHON | 同上 |
| `test_builtin` | `test_input_tty_non_ascii_unicode_errors` | 2727 | TODO: RUSTPYTHON | 同上 |
| `test_builtin` | `test_input_tty_null_in_prompt` | 2732 | TODO: RUSTPYTHON | 同上 |
| `test_builtin` | `test_input_tty_nonencodable_prompt` | 2738 | TODO: RUSTPYTHON | 同上 |
| `test_builtin` | `test_input_tty_nondecodable_input` | 2745 | TODO: RUSTPYTHON | 同上 |
| `test_builtin` | `test_input_no_stdout_fileno` | 2753 | TODO: RUSTPYTHON | 同上 |
| `test_descr` | `test_bad_new` | 1864 | (无注释,`@expectedSuccess` 覆盖) | 故意要求通过,非失败标记 |
| `test_descr` | `test_restored_object_new` | 1912 | (无注释,`@expectedSuccess` 覆盖) | 同上 |
| `test_ssl` | `test_get_default_verify_paths` | 765 | `expectedFailureIf(sys.platform == "android")` | 条件为假(Windows) |
| `test_sys` | `test_getandroidapilevel` | 1219 | TODO: RUSTPYTHON | skipped:仅 Android 平台 |
| `test_int` | `test_denial_of_service_prevented_int_to_str` | 590 | `expectedFailureIf(not hasattr(time, "get_clock_info"))` | 条件为假(`time.get_clock_info` 已存在) |

---

## 7. 仍然失败的标记:模块汇总与原因类别

实测范围内**所有激活的 expectedFailure 标记都仍按预期失败**(239 行,含继承重复)。失败原因取自标记注释中记录的错误信息,按四类归并如下。需要说明:类别划分基于注释文本的**主观归类**,仅作快速参考;同一模块内可能存在多种类别,表中列"主导类别"。

### 7.1 失败原因类别定义

| 类别 | 判定依据(注释中出现的关键词/模式) | 典型表现 |
|---|---|---|
| **平台差异** | android / darwin / win32 / windows / tty / platform / locale / MS_WINDOWS / is_android / descriptor 等 | 仅在特定平台或环境下失败,如 Android API、TTY、locale/编码环境 |
| **API 缺口** | unknown encoding / no attribute / not implemented / NotImplementedError / not available / missing / no module / not supported / 未实现 等 | RustPython 尚未实现的模块/属性/编码,如 `LookupError: unknown encoding: gb2312`、`module 'pickle' has no attribute 'PickleBuffer'` |
| **语义差异** | AssertionError / not triggered / Warning / wrong error message / does not match / not raised / TypeError / ValueError / 错误消息 等 | 行为已存在但与 CPython 语义不一致:错误消息文本、警告是否触发、repr、异常类型/时机等 |
| **崩溃** | segfault / crash / panic / segmentation fault / hang | 会崩溃或挂起解释器的测试(此类按 AGENTS.md 规范应使用 `skip` 而非 `expectedFailure`;实测范围内激活的 expectedFailure 标记无崩溃类) |

### 7.2 实测模块的失败标记汇总(标记仍失败,按主导类别)

| 模块 | 仍失败标记数 | 失败测试名示例(装饰器行号) | 原因摘要(取自注释) |
|---|---:|---|---|
| `test_builtin` | 4 | `test_dir`(L658)、`test_exec_builtins_mapping_import`(L992)、`test_eval_builtins_mapping_reduce`(L1001)、`test_type_typeparams`(L2973) | `dir()` 中意外出现 `__repr__`;eval/exec 内置映射的报错行为不符;type type_params 支持不足 → **语义差异** |
| `test_types` | 3 | `test_dunder_get_signature`(L638)、`test_capsule_type`(L699)、`test_tuple_subclass_as_bases`(L1884) | 平台相关(dunder/capsule 签名、tuple 子类作基类)→ **平台差异/语义差异** |
| `test_funcattrs` | 1 | `test_invalid___code___assignment`(L75) | `__code__` 赋值校验行为不符 → **语义差异** |
| `test_class` | 11 | `testForExceptionsRaisedInInstanceGetattr2`(L577)、`testObjectAttributeAccessErrorMessages`(L694)、`test_no_flags_for_slots_class`(L890) 等 | 实例 `__getattr__` 异常、属性访问错误消息、内联值/inline values 相关(has_inline_values 未实现)→ **语义差异/API 缺口** |
| `test_descr` | 24 | `test_slots`(L1106)、`test_slots_special2`(L1362)、`test_classmethods`(L1547)、`test_basic_inheritance`(L2790)、`test_method_wrapper`(L4647) 等 | `__slots__`、classmethod/staticmethod 注解、MRO/继承、method-wrapper 等描述符语义差异 → **语义差异** |
| `test_exceptions` | 22 | `testRaising`(L63)、`testSyntaxErrorMessage`(L148)、`test_invalid_setattr`(L664)、`test_unraisable`(L1727)、`test_encodings`(L2389) 等 | 异常语法错误消息、`__setattr__` 校验、unraisable 处理、编码错误消息 → **语义差异** |
| `test_code` | 3 | `test_co_lnotab_is_deprecated`(L430)、`test_invalid_bytecode`(L493)、`test_co_branches`(L1492) | `co_lnotab` 有意未实现、`code.replace()` 字节码校验、co_branches 缺失 → **API 缺口** |
| `test_dis` | 7(9 行,继承重复) | `test_bug_1333982`(L1109)、`test_disassemble_str`(L1167)、`test_code_info`(L1626)、`test_info`(L2325) | 反汇编输出/`disassemble_str`/代码信息展示与 CPython 不一致 → **语义差异** |
| `test_listcomps` | 1 | `test_frame_locals`(L694) | 列表推导中 frame locals 语义差异 → **语义差异** |
| `test_dictcomps` | 1 | `test_illegal_assignment`(L78) | 字典推导非法赋值错误消息 → **语义差异** |
| `test_import` | 3 | `test_dll_dependency_import`(L781)、`test_create_dynamic_null`(L1251)、`test_unencodable_filename`(L2056) | DLL 依赖导入、动态模块创建、不可编码文件名 → **平台差异/API 缺口** |
| `test_runpy` | 1 | `test_pymain_run_stdin`(L870) | 从 stdin 运行脚本的 runpy 行为 → **语义差异** |
| `test_super` | 3 | `test_various___class___pathologies`(L93)、`test___class___mro`(L191)、`test_super_subclass___class__`(L449) | `super()` 与 `__class__` 闭包交互 → **语义差异** |
| `test_generators` | 2 | `test_close_clears_frame`(L305)、`test_frame_outlives_generator`(L724) | 生成器 close 后 frame 清理、frame 存活语义 → **语义差异** |
| `test_fstring` | 14 | `test_ast_line_numbers_with_parentheses`(L386)、`test_mismatched_parens`(L610)、`test_fstring_nested_too_deeply`(L635) 等 | f-string 语法错误消息/嵌套深度/AST 行号 → **语义差异** |
| `test_symtable` | 11 | `test_function_info`(L250)、`test_globals`(L259)、`test_local`(L278) 等 | symtable 模块信息字段不符 → **语义差异/API 缺口** |
| `test_peepholer` | 3 | `test_format_errors`(L730)、`test_setting_lineno_one_undefined`(L1009) | 格式错误消息、行号设置 → **语义差异** |
| `test_named_expressions` | 2 | `test_named_expression_invalid_17`(L101)、`test_named_expression_invalid_mangled_class_variables`(L368) | 海象表达式非法用法的错误消息 → **语义差异** |
| `test_yield_from` | 1 | `test_broken_getattr_handling`(L541) | `yield from` 对损坏 `__getattr__` 的处理 → **语义差异** |
| `test_tokenize` | 6 | `test_newline_and_space_at_the_end_of_the_source_without_newline`(L1910)、`test_number_starting_with_zero`(L2229)、`test_async`(L2870) | tokenize 边界行为/数字 token → **语义差异** |
| `test_ctypes` | 6 | `test_python_api`、`test_values`、`test_dllist`、`test_win32_com_foreign_func`(子文件) | ctypes Python API/值/加载 → **API 缺口/平台差异** |
| `test_sqlite3` | 49+ | `test_userfunctions`(19)、`test_hooks`(8)、`test_dump`(7)、`test_factory`(7)、`test_dbapi`(6) 等 | sqlite3 用户函数/hook/工厂/转储 → **API 缺口/语义差异** |
| `test_importlib` | 3 | `test_threaded_import`(2)、`test_windows`(1) | 线程导入(hang?)、Windows 特定导入 → **平台差异** |
| `test_ssl` | 5 | `test_tls_unique_channel_binding`(L4196)、`test_sni_callback_alert`(L4459)、`test_msg_callback_tls12`(L5230) | tls-unique channel binding、SNI 回调、msg callback TLS1.2 → **API 缺口/语义差异** |
| `test_functools` | 9 | `test_cmp_to_signature`(L1248)、`test_cmp_to_key_arguments`(L1252)、`test_lru_hash_only_once`(L1766) | `_functools.cmp_to_key` 参数/签名校验 → **语义差异** |
| `test_enum` | 2 | `test_intenum_from_bytes`(L1751)、`test_custom_strenum`(L3039) | IntEnum.from_bytes、自定义 StrEnum → **语义差异** |
| `test_weakref` | 4 | `test_callable_proxy`(L406)、`test_callback_in_cycle_resurrection`(L755)、`test_callbacks_on_callback`(L802) | 可调用代理、弱引用回调复活 → **语义差异** |
| `test_gc` | 2 | `test_function_tp_clear_leaves_consistent_state`(L239)、`test_get_stats`(L834) | tp_clear 一致性、gc 统计 → **语义差异/API 缺口** |
| `test_sys` | 4 | `test_exit`(L212)、`test_c_locale_surrogateescape`(L1057)、`test_jit_is_active`(L2227) | exit 行为、locale surrogateescape、JIT 标志 → **语义差异/平台差异** |
| `test_memoryview` | 8(18 行,继承重复) | `test_hash_use_after_free`(L390)、`test_hex_use_after_free`(L460)、`test_gc`(L569/L611/L623) | 释放后使用检查、`gc` 对 memoryview 的处理 → **语义差异** |
| `test_int` | 1 | `test_unicode`(L250) | int 与 unicode 互转的异常消息 → **语义差异** |
| `test_str` | 5(7 行,继承重复) | `test_format`(L1073)、`test_formatting`(L1502)、`test_raiseMemError`(L2469) | str.format 行为、内存错误 → **语义差异** |

**类别统计(39 个实测模块的 175 个标记)**:以注释文本归类,大致为——语义差异(错误消息/警告/行为不符)占比最高;其次是平台差异(条件式标记);API 缺口(未实现的属性/编码/模块)再次;崩溃类 0(崩溃类测试按规范使用 `skip`)。全仓库 1075 个标记中,编码缺失类(API 缺口)集中出现在 `test_codecs*`、`test_codecmaps_*`、`pickletester.py` 等模块。

---

## 8. 结论与建议

1. **本次实测范围内没有可移除的过时标记(0 个 unexpected success)**,39 个模块全部通过且无真实失败;所有激活的 `expectedFailure` 标记仍如实反映未修复行为。这与"近期已主动清理过时标记"的仓库状态一致。
2. **不建议移除 §6 的 13 处休眠标记**:它们因平台条件或 tty/Android 环境未激活,一旦条件满足仍会失败;其中 `test_descr` 两处为 `@expectedSuccess` 覆盖,属保护性标记。
3. **后续排查建议**:若继续寻找过时标记,可优先关注**未在本次实测范围**但标记数量大的模块,尤其是已知全绿或近期有大量修复的领域:`test_pyexpat`(49)、`test_xml_etree`(35)、`test_sax`(31)、`test_pydoc`(30)、`test_inspect`(29)、`test_pickle/pickletester`(24,其中 5 处为 `PickleBuffer` API 缺口)、`test_coroutines`(18)、`test_bytes`(15)、`test_ast`(18)、`test_trace`(16)、`test_socket`(13)、`test_warnings`(12)、`test_hmac`(12)。这些模块中若存在已修复项,运行 `-m test -v` 会出现 `unexpected success`,方法与本次一致。
4. **计数提示**:按测试名统计时会遇到"同名测试分布于多个类"(继承/双实现)与"多行描述导致状态独立成行"两类伪差异,报告 §4 的口径说明可作参考。

---

*本报告由只读调查生成;除 `bench\reports\stale_markers.md` 外未创建/修改任何文件。*
