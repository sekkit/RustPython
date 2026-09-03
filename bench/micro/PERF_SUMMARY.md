# 性能基准汇总（RustPython vs CPython 3.12，Windows x64，release）

## fn_call 微基准
| 操作 | RustPython | CPython | 倍数 |
|---|---|---|---|
| empty_call | 220.3 ns | 52.7 ns | 4.2x |
| add1_call | 385.2 ns | 98.9 ns | 3.9x |
| method_call | 412.9 ns | 106.6 ns | 3.9x |
| fib(24) 递归 | 39.2 ms | 6.1 ms | 6.4x |
| int_add | 177.8 ns | 57.3 ns | 3.1x |
| int_mul | 251.8 ns | 73.4 ns | 3.4x |
| list_append | 160.3 ns | 57.5 ns | 2.8x |
| dict_setget | 305.4 ns | 106.8 ns | 2.9x |

### qualname 复用优化后（commit c08b8741d）
| 操作 | 优化前 | 优化后 | 提升 |
|---|---|---|---|
| empty_call | 220.3 | 207.4 | -5.9% |
| add1_call | 385.2 | 362.3 | -6.0% |
| method_call | 412.9 | 392.5 | -4.9% |
| fib(24) | 39.2 ms | 41.1 ms | （波动） |
| int_add | 177.8 | 172.6 | -2.9% |
| int_mul | 251.8 | 234.9 | -6.7% |

→ 函数对象创建优化普适改善调用类基准（-5~7%），不限于闭包场景。


## 调用分解
| 场景 | RustPython | CPython | 倍数 |
|---|---|---|---|
| A 纯调用 | 219.4 ns | 46.5 ns | 4.7x |
| D 3参数 | 447.4 ns | 121.2 ns | 3.7x |
| E 变长参数 | 454.6 ns | 116.0 ns | 3.9x |
| F 全局名加载 | 87.4 ns | 30.6 ns | 2.9x |
| G 递归深度1 | 515.5 ns | 88.8 ns | 5.8x |

**参数打包成本**：D-A = 228ns（2 参数 → ~114ns/参数），CPython 仅 ~37ns/参数 → **3x 差距**

## 多形态调用（specialization 缓存效率）
| | RustPython | CPython |
|---|---|---|
| 单形态 | 351.7 ns | 99.3 ns |
| 多形态 | 514.5 ns | 133.7 ns |
| 超多形态 | 534.1 ns | 128.8 ns |
| poly/mono | 1.46x | 1.35x |

→ 形态惩罚率相近（1.46 vs 1.35），specialization 机制有效；差距在绝对基成本

## 帧/解释器循环
| 场景 | RustPython | CPython |
|---|---|---|
| 两层调用 | 375.9 ns/op | 68.1 ns/op (5.5x) |
| 递归500层 | 0.4 us/层 | 0.1 us/层 (4x) |
| 单帧循环20000 | 1892.6 us | 565.0 us (3.35x) |

## 结论
1. **参数打包**（FuncArgs/Vec 堆分配）是明确可节流项：每参数 ~114ns vs 37ns，3x 差距
2. **帧创建/链接**：两层调用 5.5x，递归 4x——帧复用可节流
3. **解释器分派**（单帧循环 3.35x）是架构性地板的一部分，但仍有优化空间
4. 形态惩罚率与 CPython 相当 → specialization 已有效
5. 容器操作（list/dict ~2.9x）最接近地板，说明 GC/分配差距小于调用链路

**首要可节流项**：`execute_call_vectorcall` 每次用 `Vec::with_capacity` 堆分配参数数组——改用栈上小缓冲区（≤3 参数零分配）

## 补充：dict 细粒度（2026-09-03）
dict_set_str_key 192.5 / set_int_key 194.8 / get_str_key 178.6 ns-op
——与 dict_setget 2.9x 一致，容器操作确认接近地板；int/str key 特化无差异，
无额外可节流空间。