# 字节码指令级成本对比（RustPython vs CPython 3.12，ns/iter）

| 操作 | RustPython | CPython | 倍数 |
|---|---|---|---|
| while_counter（LOAD/STORE+INC） | 111.6 | 28.2 | 4.0x |
| for_int_add（FOR_ITER+BINARY_OP） | 94.0 | 31.1 | 3.0x |
| compare_lt（COMPARE_OP） | 161.2 | 40.0 | 4.0x |
| load_attr（LOAD_ATTR） | 120.5 | 33.4 | 3.6x |
| call_method（CALL_METHOD） | 234.9 | 49.6 | 4.7x |
| list_subscr（BINARY_SUBSCR） | 158.1 | 43.0 | 3.7x |
| try_pass（SETUP/异常块） | 107.5 | 31.6 | 3.4x |
| for_iter10（FOR_ITER×10） | 930.1 | 275.5 | 3.4x |

## 判定
- 指令级差距集中在 **3.0-4.7x**，与解释器分派总体（3.35x）一致
- call_method 4.7x 最高——方法调用链（LOAD_METHOD+CALL）叠加分派与绑定成本
- compare_lt 4.0x——多态比较分派
- 全部属于**架构性分派地板**（专家分派器/内联缓存深度优化），无单个可低风险节流项
- 与 fn_call/帧基准结论一致：调用链与多态分派是主要差距，容器次之