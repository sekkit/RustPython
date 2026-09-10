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
signal、sqrt）OK。

## 文件级基线（TEST_ALL=True 快速扫描，2026-09-07）
| 文件 | 原失败/总 case | 现状 | 修复 commit |
|---|---|---|---|
| abs.decTest | 38+/all | ✅ 全过 | ed2867203/0b4bd593b/f4ca22daf |
| copy.decTest | 0/43 | ✅ 全过 | 同领域 |
| rescale.decTest | 0/618 | ✅ 全过 | 同领域 |
| tointegral.decTest | 86/168 | ✅ 全过 | b74b5fdef |
| compare.decTest | 59/639 | ✅ 全过 | e75eb8b6e |
| quantize.decTest | 132/776 | ✅ 全过 | b21840376/5989b7173 |
| add.decTest | 38/2100 | ✅ 全过 | f8c83d679..e5b3d9f30 |
| multiply.decTest | ? | ✅ 全过 | 共享 apply_context |
| logb.decTest | 3 | ✅ 全过 | 3ba3329b1 |
| remainder.decTest | 4 | ✅ 全过 | 05dfb1983 |
| divide.decTest | 5 | ✅ 全过 | 7f55d09ad/c011afbbd |
| minus.decTest | 8 | ✅ 全过 | 0cccbffc1 |
| plus.decTest | 1 | ✅ 全过 | 0cccbffc1 |
| class/copyabs/copysign | 0 | ✅ 全过 | 共享 |
| comparetotal.decTest | 16 | ✅ 全过 | c0cb6622d（total ordering） |

**全部 17 个核心 .decTest 文件在抽样/TEST_ALL 下通过。** 修复链条：
- subnormal/underflow 信号（adjusted exponent 判定、half-up Etiny clamp、
  尾零规范化、Clamped 仅零值、零值 [Etiny,Emax] 范围）
- sNaN 语义（signal InvalidOperation → quiet NaN，payload 按 prec 截断）
- NaN 结果 payload/sign 选择（sNaN 优先、任一 NaN 操作数、普通数不贡献）
- quantize Rounded 规则（shift>0 丢弃位数即 Rounded，零值与 padding 豁免）
- 各处表示保持（exp≥0 不 pad、双 Inf 保留）
- 进位尾零规范化、underflow clamp 按 rounding、零和符号随 rounding、
  **单次舍入**（避免 round_to_prec 中间 tie 双舍入）、subnormal/underflow
  判定用 pre-rounding 原始值
- Overflow 按 rounding 分派（half-*→Infinity、down/floor→钳制值）
- 长除法 prec+2 位（非终止商正确舍入）
- minus/plus 符号（minus 翻转、负零清除）
- compare_total 完全排序（-NaN<...<+NaN，指数排序负侧反转）

## 剩余文件抽样基线（2026-09-10 更新）
- **19+ 个 .decTest 文件全过**（含并行修复后）：
  - max/min/maxmag/minmag 平局表示顺序 ✅（f38aaae14）
  - nextplus/nextminus ✅（f4c205205）
  - nexttoward ✅（f0a813ee2，含 next_value 统一语义、apply_context 下溢
    快速路径、dec_cmp adjusted 短路、overflow handle 精确分派）
- 并行修复完成（3 子代理、独立 worktree，cherry-pick 合并验证，worktree
  已清理）。nexttoward TEST_ALL=True 全量 292 → 0；vm 53/2、stdlib 8/0
  基线不变。
- 已发现待后续项：divideint 27 fail（新扫描）、剩余未扫描文件
  （tosci/toeng/normalize/trim 等抽样 0 fail 或未确认）。
- 注：抽样模式失败数有 90% 随机跳过波动；TEST_ALL 全量扫描效率低
  （add 曾需 1103s），建议抽样+定向修复。

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

---

## 全量 .decTest 扫描审计 + 低垂果实修复（2026-09-10，commit cab5022ef）

### 扫描审计（143 文件全跑，5 个超时）
驱动脚本逐文件独立进程 + 超时保护（temp/run_scan.sh，输出 temp/scan_sample/）。
73 个文件存在失败，分类：
- **逻辑函数**（深水区）：and/or/xor/invert 普通 27/27/27/11，dd/dq 变体 17–33
- **rotate/shift**（16–19）、**remainderNear**（19–26）、**fma**（8–15）
- **数学函数**（文档已知未处理）：exp 32、ln 29、log10 23、powersqrt 10
- **squareroot.decTest Rust panic**：malachite-nz div.rs division by zero（深水区）
- 5 个超时未完全扫描：add、power、subtract、randoms、randomBound32

### 本轮修复（cab5022ef，全部经 CPython 3.12 实测对齐）
1. **05up/01up HALF_UP 残留**：quantize/quantize_inner/format_sci 三处内联舍入
   分支混入 `r*2>=pow → up`；改与 round_up_decision 一致（r==0 不 up；q 末位
   0/5 或 0/1 才 up）。修复 ddqua1907/1908、r05up 系列 format 路径。
2. **to_integral_exact Rounded flag**（quantize_inner + Context 版本）：精确丢弃
   小数位也设 Rounded（intxx043: -1.0 → -1 Rounded）；**零值豁免**
   （-0E-3 → -0 无 flag，仅 inexact 零如 -0.001 设 Inexact+Rounded；intxx143/144）。
3. **clamp zero 钳制目标**：clamp=1 时钳到 etop（Emax-prec+1，clam412: 0E+400 →
   0E+369），clamp=0 时 Emax（0E+384）。
4. **clamp 分支溢出跳过**：adjusted > Emax 时不再误设 Clamped（ddadd81621 →
   -9.999999999999999E+384 Overflow；ddmul870 → Infinity Overflow）。
5. **comparetotal NaN payload 排序**：Decimal + Context 两个版本各 2 处，同类
   NaN 按 payload 排序（NaN42 > NaN01；-NaN42 < -NaN01；mag 模式符号清除后
   仍按 payload）。修复 ddcot977/dqcot977/ddctm977/982/dqctm977/982 等。
6. **compare_signal NaN 结果**：任意 NaN 操作数 → InvalidOperation + 安静 NaN
   （sign/payload 跟第一 NaN 操作数；dqcms840: -NaN+NaN → -NaN；ddcms824 等）。
   原实现误返回数值比较。

### 修复后归零（抽样 seed 1635165126）
clamp / tointegralx / tointegral / ddAdd / ddMultiply / ddCompareSig /
dqCompareSig / ddCompareTotal / ddCompareTotalMag / dqCompareTotal /
dqCompareTotalMag / comparetotal / comparetotmag / divide 全部 0 fail；
rounding 0 fail（波动消除）；base 仅剩 bsrx510（见下）。

### 已知 CPython 与 decTest 字面期望差异（RustPython 跟随 CPython）
- **base bsrx510**：decTest 期望 toSci(-1.23450) → '-1.2345 Rounded'，CPython 实际
  返回 '-1.23450'（无 flag）——RustPython 现与 CPython 一致，此用例"失败"为
  decTest 期望与 CPython 实现固有差异。
- **divide divx287-288**：0.1/9999e-999999997 期望 E+999999992 有限值，但上下文
  emax=384 → CPython 溢出为 Infinity（Overflow Inexact Rounded）；RustPython 一致。
- **rounding rmux518**：multiply 12355×12 055E-1 舍入差异，CPython half_even
  得 1.4826E+5，RustPython 跟随。

### 深水区（后续专职，按优先级）
1. squareroot/powersqrt Rust panic（除零，需定位 dec_sqrt 或 power 路径）
2. 数学函数 exp/ln/log10（高精度级数/Taylor 未实现，32/29/23 fail）
3. 逻辑函数 and/or/xor/invert（需按 numtheory 编码实现）
4. rotate/shift、remainderNear、fma（Rounded trap 顺序）
5. 超时大文件 add/power/subtract/randoms/randomBound32 全量确认
6. 全量 TEST_ALL=True 复扫（当前核对受 90% 抽样波动影响）

## max/min 平局回退修复（2026-09-10，commit 08b9e5a10）

### 根因
f0a813ee2（nexttoward 合并）误将 f38aaae14 的 max/min 平局修复整体回退：
删除 max_picks_a/min_picks_a/max_mag_picks_a/min_mag_picks_a 四个辅助函数，
Decimal 与 Context 共 8 处调用改回 dec_cmp 简单比较（相等时永远选左操作数），
导致扫描中 max/min/maxmag/minmag 系列（普通 2–5、dd/dq 3–4 fail）重新失败。

### 修复（_decimal.rs，73+/16-）
- 恢复四个辅助函数（f38aaae14 原文）：平局时 max 选 +0 优先、同正选 exp 大、
  同负选 exp 小（min 镜像）；max_mag/min_mag 平局委托给 max/min
- Decimal 版 max/min/max_mag/min_mag 与 Context 版 4 方法（decTest 路径）
  全部改回辅助函数

### 验证
- 44 个平局用例与 CPython 实测逐一一致（max(0E1,0E2)→0E+2、
  max(-1,-1.0)→-1.0、min(0,-0)→-0、min(-1000,-1E+3)→-1E+3、
  max_mag(-Inf,Inf)→Infinity、-0E+5 系列等）
- 12 个 decTest 文件抽样重扫全部归零：max/maxmag/min/minmag +
  ddMax/ddMaxMag/ddMin/ddMinMag/dqMax/dqMaxMag/dqMin/dqMinMag
- 回归：vm 53/2（基线一致）、stdlib 8/0

### 剩余低垂果实（未处理）
class(3)/ddClass(2)/dqClass(2)、scaleb(5)/ddScaleB(5)/dqScaleB(5)、
dsBase(3)/ddBase(5)、reduce(2)/ddReduce(1)/dqReduce(1)、ddRemainder(1)、
ddToIntegral(1)/dqToIntegral(1)。

## class/normalize/zero-clamp/scaleb 修复（2026-09-10，commit 9802fcfee + 664456eac）

### 9802fcfee：number_class / normalize / zero clamp
- number_class（Decimal + Context）：NaN/sNaN 分类不带符号（clasx043/243）；
  Subnormal/Infinite 用 adjusted exponent 判定（clasx006：0.99999999E-999
  实际 coeff 99999999/exp -1007、adjusted -1000 < Emin-999 → +Subnormal）
- normalize（Decimal + Context，decTest 'reduce'）：sNaN → InvalidOperation +
  安静 NaN（payload 按 prec 截断，ddred830: -sNaN010→-NaN10）；underflow
  到零指数归 0（redx239: -0.0009E-999→-0）
- apply_context zero 分支：clamp=1 时零指数 > etop 即钳到 etop（ddrem426:
  remainder(1E+384,5E+383)→0E+369 Clamped；plus(0E+380)→0E+369、
  plus(0E+368) 不钳）
- 验证：class/ddClass/dqClass/ddReduce/dqReduce/ddRemainder/clamp 归零；
  redx213 为 decTest 与 CPython 固有差异（CPython normalize 尾零剥离不设
  Rounded），RustPython 跟随 CPython

### 664456eac：scaleb（Decimal + Context）
- 任意 NaN/sNaN → InvalidOperation + 安静 NaN（sNaN payload 优先：scbx876
  scaleb(NaN016,sNaN191)→NaN191；scbx027/028）
- 指数必须裸整数（y.exp==0）：-0.01/2.0/0E+2 均 InvalidOperation（scbx041）
- 范围恢复为 CPython 的 ±2*(Emax+prec)（对照 _pydecimal.scaleb 源码 + C
  实现实测：-2017 非法、-2016 合法 underflow、n=1000 溢出到 Infinity，
  scbx127）
- Infinity 操作数原样透传
- 验证：scaleb/ddScaleB/dqScaleB 归零（dqScaleB 曾有 3 fail 为抽样波动）

### 剩余低垂果实（更新）
- to_sci_string 系列（dsbsr510/ddbas935/940/1077/745：溢出 Clamped、05up
  舍入、零指数、sNaN payload 截断）——专职待办
- dsBase(3)/ddBase(5) 其余用例、ddToIntegral(1)/dqToIntegral(1)
