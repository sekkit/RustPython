# SignalDict 语义评估 — test_decimal 3 个 Whitebox xfail 根因

> 状态：**已完成根因判定**。3 个 xfail 均为 mpdecimal `SignalDict` 语义缺口，
> 属"实现有问题（可修复的专用类型）"，需在 `_decimal.rs` 实现带状态标志的
> dict 子类。尚未实施（评估与 numpy/code-extra 并行，优先级排后）。

## 涉及测试（Lib/test/test_decimal.py，Whitebox/CPython-only 分支）

| 行号 | 测试 | 根因 |
|---|---|---|
| 5065 | `test_c_context_errors` | SignalDict 输入校验：`c.flags.__setitem__(801, 0)` → KeyError；`__delitem__(Overflow)` → ValueError；`setattr(c,'flags',['x'])` → TypeError；`setattr(c,'flags',{0:1})` → KeyError；含非法 key 的信号 dict 赋值 → KeyError |
| 5753 | `test_c_disallow_instantiation` | `type(C.localcontext())`（ContextManager）应禁止直接实例化（DISALLOW_INSTANTIATION） |
| 5758 | `test_c_signaldict_segfault` | 未初始化的 SignalDict 实例（`SignalDict()` 直接构造）所有操作抛 `ValueError: invalid signal dict` |

## 当前实现状态
- `crates/stdlib/src/_decimal.rs`（2555-2560 行）：`PyContext` 的 `flags`/`traps`
  字段是 `PyMutex<PyDictRef>`（普通 dict），初始化时填充 9 个信号异常类型键。
- `flags`/`traps` 属性访问（2693-2742 行）：返回普通 dict 克隆 / 校验赋值。
- 无 `SignalDict` 类、无 `DISALLOW_INSTANTIATION` 标志。

## CPython/mpdecimal 语义（要达成的目标）
1. **SignalDict = dict 子类**：`C.Context().flags` 返回 SignalDict 实例。
   SignalDict 内部持有指向所属 `mpd_context_t` 的指针（或未初始化标志）。
2. **输入校验**（`test_c_context_errors`）：键必须是 9 个信号异常类型之一
   （否则 KeyError），值布尔；`__delitem__` 只允许信号键；delete 非信号键 →
   ValueError（mpdecimal 对 flags/traps 的 del 有特殊语义）。
3. **未初始化实例**（`test_c_signaldict_segfault`）：`SignalDict()` 直接构造 →
   内部上下文指针为 NULL → len/iter/repr/getitem/setitem/eq/copy 全部抛
   `ValueError: invalid signal dict`（gh-106263 防 segfault 语义）。
4. **DISALLOW_INSTANTIATION**（`test_c_disallow_instantiation`）：`type(C.localcontext())`
   即 `localcontext` 返回的 ContextManager 类型，`__new__` 应抛
   `TypeError: cannot create 'X' instances`（cpython DISALLOW_INSTANTIATION 语义）。

## 实现方案（中等工程，~120-200 行）
- 在 `_decimal.rs` 定义 `SignalDict` pyclass（`#[pyclass]`，`base="dict"` 或手动
  注册为 dict 子类），含 `ctx: PyMutex<Option<NonNull<PyContext>>>` 状态；
  覆盖 `__len__/__iter__/__repr__/__getitem__/__setitem__/__delitem__/__eq__/copy`
  等方法，未初始化时统一抛 `ValueError("invalid signal dict")`，已初始化时做
  SignalDict 键校验后转发到内部 dict。
- `PyContext::flags/traps` getter 改为返回绑定到 self 的 SignalDict。
- ContextManager 类型加 `#[pyclass(... )]` 禁止实例化（或 `__new__` 抛 TypeError）。
- 风险控制：改动后重跑 `-m test test_decimal` 确认 187/187 不回归；
  若 SignalDict 子类化影响 dict 协议，仅对 flags/traps 使用，不动其它。

## 判定
- 属"实现有问题（可修复）"，非架构墙。
- 但改动 `_decimal.rs` 核心类型（flags/traps 返回类型变化），有回归风险；
  且与 numpy（已获突破）、code-extra（实施中）相比优先级靠后。
- 结论：**已判定、方案明确，待专职排期实施**（与 DELIVERABLES 清单一致）。
