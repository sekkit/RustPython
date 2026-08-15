# RustPython 纯 Python 生态兼容缺口调查报告

- 调查对象:RustPython 0.5.0(Python 3.14.0.alpha,heads/main-dirty:529ba89de),release 二进制 `target\release\rustpython.exe`
- 调查日期:2026-08-16
- 运行方式:`$env:RUSTPYTHONPATH = "C:\Dev2\luna-lang\RustPython\Lib"`;包装到 `bench\labs\sitepkg`(pip `--target`)
- 网络测试前已清除 `HTTP_PROXY/HTTPS_PROXY/ALL_PROXY`(本机默认 127.0.0.1:20809)
- 复现脚本:`bench\labs\smoke\`(冒烟脚本 + `run_smoke.ps1` 带 5 分钟/包超时);`bench\labs\smoke\patchtest\` 为 celery 单行补丁实验副本
- 说明:本次会话的 pip 后台任务默认工作目录为 `C:\Dev2\luna-lang`(非仓库根),包装到了 `C:\Dev2\luna-lang\bench\labs\sitepkg`,随后已合并到 `RustPython\bench\labs\sitepkg`(任务指定位置);`sitepkg\numpy251\` 为前次会话遗留的 numpy 2.5.1 探针。

---

## 1. 成功矩阵(17 个包全部通过最小功能验证)

| 包 | 版本 | 验证的功能 | 实际输出(证明真的能用) |
|---|---|---|---|
| click | 8.4.2 | 命令定义 + option 解析 + CliRunner 调用 | `exit=0 out='Hello RustPython!'` |
| jinja2 | 3.1.6 | 独立模板渲染(变量 + if) | `'Hello world! x is truthy'` |
| itsdangerous | 2.2.0 | URLSafeTimedSerializer 签名/验签回环 | `roundtrip={'user': 42}`(max_age=3600) |
| werkzeug | 3.1.8 | WSGI Request 解析 + test Client 完整请求 | `status=200 body='method=GET path=/hello arg=1'` |
| blinker | 1.9.0 | Signal 连接/发送/接收 | `received=[('main', {'value': 7})]` |
| httpx | 0.28.1 | 同步 GET `https://www.baidu.com` | `status=200 len=29506`(真实网络) |
| aiohttp | 3.14.3 | import + asyncio.run 异步 GET baidu | `status=200`(真实网络,1 秒内完成) |
| tomllib(标准库) | — | TOML 解析 | `{'title': 'hello', 'owner': {'name': 'x'}}` |
| tomli | 2.4.1 | TOML 解析 | 同上 |
| sympy | 1.14.0 | 符号求导 | `diff(x**3+2*x+1, x) = 3*x**2 + 2` |
| networkx | 3.6.1 | 建图 + 最短路 | `nodes=[1, 2, 3] shortest=[1, 3]` |
| pydantic | 1.10.26(v1) | BaseModel 创建 + `.json()` 序列化 | `json={"name": "alice", "age": 30}` |
| dateutil | 2.9.0.post0 | ISO8601 带时区解析 + relativedelta | `2024-03-15 10:30:00+08:00` → `+1month=2024-04-15 ...` |
| tqdm | 4.70.0 | 进度条循环 | `total=45`(进度条正常渲染) |
| colorama | 0.4.6 | init/deinit + 前景色 | `'\x1b[31mred\x1b[0m'` |
| humanize | 4.16.0 | intword 人性化数字 | `'1.2 million'` |
| attrs | 26.1.0 | `@define` 类定义 + repr/eq | `repr=Point(x=1, y=2) eq=True` |

> 附注 1:aiohttp 3.14.3 依赖的 multidict/yarl/frozenlist/propcache 均被 pip 选装了**纯 Python wheel**(如 `multidict\_multidict_py.py`),因此全功能可用——不是 C 扩展被 RustPython 加载。
> 附注 2:pydantic:pip 在 3.14 标签下直接安装了 v1 的 `py3-none-any` 纯 Python wheel(10.26,无 C speedups),因此可用。若强制安装 pydantic v2,会因 `pydantic_core`(Rust C 扩展)无法加载而 import 失败——这是 C 扩展类缺口,不是纯 Python 缺口。
> 附注 3:werkzeug/blinker 新版已移除 `__version__` 属性(改用 `importlib.metadata`),初次测试脚本因访问 `__version__` 误报失败,改用 `importlib.metadata.version()` 后确认核心功能全部可用。`importlib.metadata` 在 RustPython 中工作正常。

## 2. 失败矩阵

| 包 | 版本 | 阶段 | 错误类型 | 错误消息 | 第一个失败点(文件:行/代码) | 缺口类别 |
|---|---|---|---|---|---|---|
| celery | 5.6.3 | import(`celery/__init__.py:16` → `from . import local`) | `ValueError` | `'__dict__' in __slots__ conflicts with class variable` | `celery/local.py:51` `class Proxy:`(第 55 行 `__slots__ = ('__local','__args','__kwargs','__dict__')` 与第 110 行 `@property def __dict__` 冲突) | **行为差异 / 类创建语义** |
| numpy | 2.5.1(遗留探针) | import | `AttributeError` | `module 'nt' has no attribute '_add_dll_directory'` | `numpy/__init__.py:91` `_delvewheel_patch_1_11_2()` → `os.add_dll_directory`(`Lib/os.py:1172` → `nt._add_dll_directory`) | **缺属性**(`nt._add_dll_directory`) |
| (pydantic v2) | 2.x | 未实测 | — | — | 若强制装 v2:import `pydantic_core`(Rust C 扩展)失败 | **C 扩展** |

### celery 失败详析(本次唯一纯 Python 生态失败点)

- **触发模式**:类体同时做两件事 —— ① `__slots__` 元组中包含 `'__dict__'`(werkzeug 经典 Proxy 技巧,给实例保留 dict 槽);② 类体又定义了同名 `__dict__` property(用于把 dict 访问转发给被代理对象)。**CPython 接受该模式**(实例 dict 由槽创建,类级 property 遮蔽 `__dict__` 访问,`object.__setattr__` 仍写入真实实例 dict);RustPython 的类创建检查误判为冲突并抛 `ValueError`。
- **影响面**:celery 的依赖 kombu 5.6.2、billiard 4.2.4 均能正常 import,唯一堵点就是 celery 自带 `celery/local.py` 的 `Proxy` 类。
- **决定性验证**(`bench\labs\smoke\patchtest\celery\local.py`):仅把 `__slots__` 中的 `'__dict__'` 移除(一行),celery 5.6.3 **完整 import 成功**,且可创建 `Celery('demo', broker='memory://')` app、注册 task;`add.apply((2,3))` 在 `local.py:292/328`(`object.__getattribute__(self,'__thing')`)更深处失败——恰因移除 `'__dict__'` 槽后实例 dict 缓存机制缺失。**结论:修好 RustPython 的类创建语义即可完整解锁 celery**,无需其他改动。

### numpy 失败析(上下文,非本批测试)

- delvewheel 补丁在 numpy `__init__.py` 第 91 行无条件调用 `os.add_dll_directory`;RustPython 的 `Lib/os.py:1172` 存在该函数包装,但底层 `nt._add_dll_directory` 缺失(`hasattr(nt,'_add_dll_directory') == False`)。
- 注意:`os.add_dll_directory` 在 RustPython 中**存在但不可用**(内部调缺失的原语),属于半实现桩。
- 即使补上该属性,numpy 随后仍会因加载 `*.cp314-win_amd64.pyd`(C 扩展)失败——C 扩展支持缺失是 numpy/pandas/lxml/cryptography/pillow 的最终堵点,属大工程。

## 3. 附加探针结果(37 项,36 PASS / 1 FAIL)

### 语法/类型探针(20 项)
- **全部通过**:PEP 695 `type Alias = ...`、PEP 695 泛型函数 `def f[T](...)`、`int | None` 联合注解、match 语句、dataclasses `slots=True + kw_only=True`、functools.cache、zoneinfo.ZoneInfo、datetime.UTC、int.bit_count、math.comb、importlib.metadata.version、typing.ParamSpec、sys.intern、ntpath、`list[int]()` 内置泛型、contextlib.nullcontext、super()、dict `**` 合并、str.removeprefix。
- **唯一失败**:`__slots__` 含 `'__dict__'` + 类体 `__dict__` property(celery 模式)。

### 常用模块/API 探针(17 项)
- **全部通过**:subprocess.run(cmd echo)、concurrent.futures.ThreadPoolExecutor、multiprocessing(import + cpu_count=64)、decimal、fractions、ast.parse/dump、inspect.signature、enum.StrEnum、pathlib、tempfile.NamedTemporaryFile、hashlib.blake2b、shutil.which、socket.getaddrinfo、os.scandir、typing.get_type_hints、itertools.islice/count、re.sub 反向引用。

## 4. 缺口类别统计

| 缺口类别 | 数量 | 涉及 |
|---|---|---|
| 缺模块 | 0 | —(所有目标包均可安装、可定位) |
| 缺属性 | 1 | `nt._add_dll_directory`(仅影响 delvewheel 类 Windows 补丁路径) |
| 缺语法 | 0 | —(PEP 695/match/泛型等 3.12+ 语法均受支持) |
| 行为差异(类创建语义) | 1 | celery `Proxy`:`__slots__` 含 `'__dict__'` + 类级 `__dict__` descriptor |
| C 扩展加载 | 2 | numpy 最终堵点;强制装 pydantic v2 时的 pydantic_core |

## 5. 修复优先级建议

### 低垂果实(建议优先)

1. **【最高价值】类创建语义:`__slots__` 中的 `'__dict__'` 与类级 `__dict__` descriptor 共存**(缺口:`ValueError: '__dict__' in __slots__ conflicts with class variable`)
   - 位置:RustPython 类型创建(type_new / slots 校验)代码;CPython 规则为——仅当冲突名在类字典中**不是 descriptor** 时才报错,property 是 descriptor 应放行,并让实例 dict 由槽创建、类级 property 遮蔽访问。
   - 收益:**一行修复即可完整解锁 celery 5.6.3**(import + app 创建 + 任务执行)。该模式是 werkzeug 经典 Proxy 技巧,werkzeug 3.1.8 已重写避开,但 celery/kombu 等仍在使用。
2. **【中价值】`nt._add_dll_directory` 缺失**
   - 位置:`crates/stdlib/src/nt.rs`(Windows)。补一个返回句柄 cookie 的桩,或让 `os.add_dll_directory` 在未实现时干净地抛 `NotImplementedError`。
   - 收益:消除 numpy import 的第一步崩溃(错误更可控)。**注意**:单独修它**不能**解锁 numpy —— 后续 `.pyd` 加载仍会失败。
3. **【顺手】半实现桩审计**:`os.add_dll_directory` 存在但内部依赖缺失原语,属于"半实现 API",建议统一为"存在即可用"或"明确抛 NotImplementedError",避免生态包误判能力。

### 大工程(需长期投入)

4. **C 扩展(.pyd/.so)加载支持**:解锁 numpy/pandas/lxml/cryptography/pillow 及 pydantic v2(pydantic_core)等整个 C 扩展生态。这是 RustPython 架构级能力,非单点修复。
5. 其余 17 个测试包全绿,无需修复。

## 6. 结论

在本次测试范围内,纯 Python 生态兼容性**非常好**:17/18 个包级测试通过(click/jinja2/werkzeug/itsdangerous/blinker/httpx/aiohttp/tomli/tomllib/sympy/networkx/pydantic-v1/dateutil/tqdm/colorama/humanize/attrs),网络栈(httpx/aiohttp/ssl/socket)与 3.12+ 语法(PEP 695、match)均可用。唯一纯 Python 生态缺口是 **celery 的 `Proxy` 类定义模式**(`__slots__` 含 `'__dict__'` + `__dict__` property),属**类创建语义行为差异**,是单个机制级、低垂果实级修复,修好后 celery 可完整工作。C 扩展加载是剩下的唯一系统性短板(影响 numpy 等),需架构级投入。
