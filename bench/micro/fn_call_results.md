# fn_call 微基准：RustPython vs CPython 3.12（Windows x64，release 构建）

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

## 初步观察
- 函数调用（含参数传递/栈帧）是最大差距簇：3.9-4.2x；递归 fib 因叠加帧开销到 6.4x
- 纯运算符 op（int_add/mul）约 3.1-3.4x，含部分分派成本
- 容器（list/dict）约 2.8-2.9x，最接近——说明 GC/内存分配差距小于调用链路
- 结论倾向*架构性分派地板*为主（帧创建、参数打包、名称解析、多态 binop），
  但仍有可节流项：vectorcall 直传、实例 freelist、slot 去重、帧复用