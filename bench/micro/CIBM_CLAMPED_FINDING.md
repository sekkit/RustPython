# CIBM Clamped 信号缺陷 — 新发现（预先存在）

> 状态：**已确认为预存缺陷**（三连关前基线同样失败：120 failures vs 当前 119）。
> 因 test_decimal IBM 测试 **90% 随机抽样**（TEST_ALL 默认 False，`random.random() < 0.90`
> 跳过），不同 seed 失败数不同（119/115/120），此前 187/187 是特定 seed 抽样
> 恰好全部通过的结果。**非本会话回归。**

## 现象
- `CIBMTestCases`（C/_decimal 实现）大量 FAIL；`PyIBMTestCases`（纯 Python）全 ok。
- 失败 case 例：`absx235 abs -0.999E-999 -> 1.00E-999 Inexact Rounded Subnormal Underflow`
  ——期望**不产生 Clamped**，C 实现产生了 `decimal.Clamped`。
- eval_equation 设所有 traps=1（捕获 bug）后按期望 exceptions 关闭；Clamped 意外
  置位即 raise。

## 根因方向
- C 实现（`_decimal.rs`，Rust/mpdecimal 语义）与 P 实现（`_pydecimal`，纯 Python）
  在 **subnormal 运算的 Clamped 信号设置** 上存在差异：abs/subnormal 结果精确落在
  规格化边界时应设 Clamped（"coefficient adjusted to fit Emax/Emin"），C 实现
  在此 case 误设（或 P 实现按 CPython 语义不设）。
- 具体差异点未深挖（直接复现需文件级 context + 异常类对象 traps 键，单行脚本
  复现失败；IBM 测试按 .decTest 文件加载数据）。

## 判定
- 预存、可修复（需对比 mpdecimal 与 _pydecimal 在 subnormal/Clamped 的边界逻辑），
  属 _decimal.rs 算术语义补齐项。
- 因 IBM 90% 抽样，历史上一直"偶发通过/偶发失败"，未被识别为缺陷。
- 修复方案建议：以固定 seed（1635165126）跑 test_decimal 复现，定位 abs/subnormal
  的 Clamped 置位逻辑，与 CPython mpdecimal 对照修正。
- 排期：优先级低于 numpy/code-extra（已完成的推进项），作为 _decimal 补齐项记录。
