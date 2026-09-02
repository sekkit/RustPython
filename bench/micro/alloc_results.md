# 实例/对象分配成本对比（RustPython vs CPython 3.12，ns/alloc）

| 操作 | RustPython | CPython | 倍数 |
|---|---|---|---|
| class_empty（无__init__） | 1156.4 | 252.4 | 4.6x |
| class_slots_pair（__init__ 2字段） | 865.5 | 390.5 | 2.2x |
| tuple2 | 267.5 | 65.8 | 4.1x |
| list1 | 223.2 | 60.2 | 3.7x |
| dict1 | 295.2 | 79.0 | 3.7x |
| closure 创建 | 823.4 | 115.2 | **7.1x** |
| generator 对象创建 | 370.9 | 85.3 | 4.3x |

## 判定
- **closure 7.1x 是最大分配差距**：闭包创建涉及捕获环境（cell）分配+闭包对象
- 普通容器分配（tuple/list/dict）3.7-4.1x——RustPython 每对象有 RwLock/额外簿记
- class 无继承 4.6x、slots 2.2x（slots 因省 dict 而接近）
- 结论：**实例分配是明确可节流领域**——instance freelist（目标已列）、
  cell/闭包批量分配、RustPython PyInner 簿记减负均有空间；
  但每项需 GC 语义回归验证，属中等工程