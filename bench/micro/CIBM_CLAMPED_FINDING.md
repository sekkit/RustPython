# CIBM 信号处理缺陷 — 新发现（预先存在）

> 状态：**已确认预存缺陷**，且本轮已修复其中两处（subnormal-only 分支
> ed2867203、adjusted-exponent underflow 0b4bd593b）。剩余尾零规范化与
> Clamped 边界记录待专职。
> 因 test_decimal IBM 测试 **90% 随机抽样**（TEST_ALL 默认 False），不同 seed
> 失败数不同（119/115/120），此前 187/187 是特定 seed 抽样恰好全部通过。
> **非本会话回归。**

## 现象
- `CIBMTestCases`（C/_decimal 实现）大量 FAIL；`PyIBMTestCases`（纯 Python）全 ok。
- 失败 case 例：`absx235 abs -0.999E-999 -> 1.00E-999 Inexact Rounded Subnormal Underflow`
  ——期望**不产生 Clamped**，C 实现产生了 `decimal.Clamped`。
- eval_equation 设所有 traps=1（捕获 bug）后按期望 exceptions 关闭；Clamped 意外
  置位即 raise。用 `td.TEST_ALL = True` 可绕过抽样完整暴露。

## 已修复（本轮，主仓）

### 1. Subnormal-only 分支（ed2867203）
- `apply_context` 只在下溢分支（new_exp < etiny）设置 Subnormal；漏掉了
  `etiny <= adjusted_exp < emin` 区间（subnormal 但不 underflow）。
- 用 **adjusted exponent**（exp + digits - 1）判定：1.00E-999（prec=3/Emin=-999）
  adjusted=-999 == Emin 不是 subnormal；1E-1000 adjusted=-1000 < Emin 是 subnormal。
- 修复 absx231/232/233/234（Subnormal flag）与 absx210（无 flag 边界）。

### 2. Underflow 用 adjusted exponent（0b4bd593b）
- 下溢判定原用存储指数 new_exp < etiny，mpdecimal 用 adjusted exponent。
  100E-1002（prec=3/Emin=-999）adjusted=-1000、Etiny=-1001 → 无 underflow；
  旧判定（-1002 < -1001）误发 Underflow。
- absx213 从 raise 变为仅表示形式差异。

### 3. 尾零规范化 + half-up Etiny clamp + sNaN 语义（f4ca22daf）
- **尾零规范化**：subnormal 结果去尾零直至存储指数达 Etiny（值不变→Rounded
  非 Inexact）；若全去尾零仍 exp<Etiny，则 half-up 舍入到 Etiny
  （Underflow+Inexact+Rounded）。
- **Clamped 仅当 clamp 后系数为零**：0.001E-999→0E-1001 Clamped；
  0.009E-999→1E-1001 无 Clamped。
- **sNaN signal**：一元 abs/minus/plus 遇 signaling-NaN 置 InvalidOperation
  并返回 quiet NaN（payload+sign 保留）；abs 对 quiet NaN 保留符号
  （-NaN22→-NaN22）、对 Infinity 去符号。
- 新增辅助 trailing_zeros_of。

## 里程碑：abs.decTest 全部通过（TEST_ALL=True）
此前首个失败固定在 absx215（0.999E-999），经上述三处修复后
`abs.decTest` 在 TEST_ALL=True（绕过 90% 抽样）下**全部 case 通过**。
验证：vm 单测 53/2 基线、rustpython-stdlib 8/0、decimal 冒烟（sNaN
signal、sqrt）OK。其余 .decTest 文件（add/sub/...）的全量收敛为
后续专职项（TEST_ALL=True 全量执行较慢；抽样模式失败数在 115-122
波动区间，无回归迹象）。

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
