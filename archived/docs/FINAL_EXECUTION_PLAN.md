# Codex Meter 最终执行计划

## 0. 文档定位

本文是 Codex Meter 后续实现的唯一主执行计划。它以最终产品效果为起点，依次定义页面、指标、数据粒度、事实来源、算法、数据库、API、实施阶段和验收门禁。

本文覆盖并取代旧执行计划中“按采集器顺序一路实现、最后再验证页面”的实施顺序，但不否定现有调研结论和已经完成的可靠代码。旧文档继续作为研究与历史记录；后续任务以本文及配套的《最终执行计划精细化审查》为准。

### 0.1 执行时的数据来源参考

实现数据采集、字段映射或接口兼容时，可以查阅 [数据来源与职责边界参考](DATA_SOURCE_REFERENCE.md)。两份文档的职责不同：

- 本文规定产品目标、系统结构、算法契约、实施顺序和门禁。
- [数据来源与职责边界参考](DATA_SOURCE_REFERENCE.md)规定每类字段从哪里取得、数据属于本机级/账号级/派生级哪一种、接口能提供什么以及不能提供什么。
- [调研结论](RESEARCH_FINDINGS.md)保存接口和真实样本的研究证据。
- [Token 使用参考维度](TOKEN_USAGE_REFERENCE.md)专门规定账号每日 Token 的延迟、日期对齐和不可比较条件。
- [最终执行计划精细化审查](FINAL_EXECUTION_PLAN_REVIEW.md)规定实现前必须满足的风险门禁。

这份参考是辅助材料，不新增产品目标、数据库规范或阶段门禁。当实现过程中发现字段、接口或历史样本与预期不一致时，再用它确认边界；不能通过猜测或默认值掩盖来源不确定性。

## 1. 最终目标和产品边界

### 1.1 两个核心问题

系统最终必须可靠回答：

1. 当前机器、当前使用者，在共享账号的官方周窗口中大约消耗了多少；账号总窗口又发生了多少变化。
2. 根据完整 Reset 窗口和可信时间区间，推算 20/100/200 美元订阅档位各自大约包含多少周 Credit。

系统还必须解释每一个数字如何得出，并允许追溯到原始观察、账号上下文、价格版本、计算版本和质量标签。

### 1.2 永久分离的四种指标

| 指标 | 单位 | 主来源 | 用途 |
| --- | --- | --- | --- |
| 本机 Token | token | JSONL | 本机事实账本、日/分钟/Session 明细 |
| 本机订阅 Credit | credit | 内部计价引擎 | 本机周占比和容量标定 |
| API 等价美元 | USD | 内部计价引擎 | 成本参考，不代表实际账单 |
| 账号官方窗口 | percent | JSONL 历史观察 + App Server 实时观察 | 账号跨机器总使用、Reset、历史趋势和未观测差值 |

`账号窗口变化` 不等于 `本机用量`。两者差值只允许标记为“未观测消耗/误差”，不能直接命名为另一台机器或另一个人的精确用量。

### 1.3 第一版范围

第一版包含：

- 三个产品页面：用量总览、容量标定、算法与数据口径。
- JSONL 与 App Server 两条生产数据链路。
- ccusage 离线、固定版本的独立对账链路。
- 单机、单 SQLite 数据库、仅监听 `127.0.0.1` 的本地服务。
- Plus、Pro、Other/API、Unknown 历史上下文。
- 20/100/200 美元容量档的候选值和人工确认值。

第一版不包含：

- 多机同步或云端合并。
- 对另一台机器用量的精确归因。
- 自动认定订阅档位或自动覆盖确认容量。
- 保存提示词、回复正文、认证 token、API Key 或完整第三方 endpoint。
- 把账号每日 Token 当成 Credit 或窗口百分比。

## 2. 最终页面和页面数据契约

### 2.1 页面一：用量总览

页面回答“现在是谁在用、用了多少、发生在什么时候和哪些对话”。

#### 顶部状态区

展示：

- 当前机器、脱敏账号、认证类型、套餐、provider 和容量档。
- 官方窗口已用、剩余、Reset 时间和倒计时。
- 本机当前窗口累计 Credit、本机估算占比。
- 未观测百分比/误差。
- 数据新鲜度、最近成功采集时间和质量摘要。

#### 日期总览

左侧日历的每个日期显示：

- 本机总 Token。
- 本机订阅 Credit。
- API 等价美元。
- 当日官方百分比变化。
- 当天账号/套餐、Fast、Reset 和质量标签。

右侧趋势图显示：

- 每日本机 Credit 柱状图。
- 官方账号 `usedPercent` 阶梯线。
- 本机窗口累计估算百分比虚线。
- 未观测/误差区域。
- Reset 和账号切换垂直分隔线。

账号已用与账号剩余互为镜像。图表只画其中一条，另一项放在卡片和 tooltip，避免重复表达。

#### 选中日期后的分钟视图

按同一时间轴显示：

- 官方百分比阶梯线。
- 每分钟 Token 增量。
- 每分钟 Credit、API 等价美元。
- 当前 Reset 窗口内的本机累计 Credit。

分钟 bucket 使用半开区间 `[minute_start, minute_start + 60s)`。没有新配额采样时允许视觉 carry-forward，但 API 必须返回 `observed=false` 和上一真实样本时间，不能制造虚假采样。

#### 选中日期后的 Session 视图

展示：

- 对话标题、原始 Session ID、主对话/子代理/fork 类型。
- 开始、结束、持续时间和最后活动时间。
- 模型组成、Standard/Fast/Unknown。
- Token 分类、订阅 Credit、Fast 附加 Credit、API 等价美元。
- 占当天本机 Credit 比例、账号上下文和质量标签。
- 原始 Session 与对话组两种统计口径切换。

#### 双源与双算法对账视图

页面一必须让用户看到“系统内部结果”和“外部验证结果”，不能只在后台留下一个通过/失败状态：

- 配额对账：JSONL rate-limit 观察 vs App Server rate-limit 观察。
- 用量对账：内部 JSONL normalizer/projector vs ccusage。
- 每一项显示两侧值、绝对差、相对差、比较状态、运行时间和版本。
- 日历格显示对账状态徽标；点进日期后显示 Token 分类、模型拆分和计价的逐项对照。
- 原始 Session 行可展开 ccusage Session 结果；对话组没有 ccusage 的直接对应项，显示其包含的原始 Session 对账汇总。

ccusage 当前只能可靠提供日级和原始 Session 级 Token/成本对照，不能提供分钟级官方配额、Reset 或账号身份。分钟视图明确显示“该粒度无 ccusage 对照”；官方配额改由 JSONL 与 App Server 相互认证。

### 2.2 页面二：容量标定实验室

页面回答“官方百分比变化对应多少本机 Credit，以及某档订阅的周容量应取多少”。Credit 是主估计单位，Token 只作模型混合和对账参考。

#### 顶部容量档案

20/100/200 美元三张卡分别显示：

- 当前 confirmed 容量。
- 生效日期和历史版本。
- 最新候选值、置信区间、样本数和离散程度。
- `unmeasured/draft/confirmed/retired` 状态。

#### Reset 窗口列表

每个 Reset 到 Reset 的窗口显示：

- 账号、套餐、limit、开始/结束和完整度。
- 官方百分比轨迹和总变化。
- 本机 Credit、Fast 附加量和 Token。
- 账号切换、价格切换、采样缺口和其他机器污染风险。
- `clean/contaminated/incomplete/incomparable` 标记。

#### 区间刷选与证据面板

上图显示官方 `usedPercent` 阶梯线，下图显示本机累计 Credit 和 Session 活跃区间。用户刷选区间后计算候选容量：

```text
candidate_capacity = local_credit / (used_percent_delta / 100)
```

单个 1% 跳变不得作为高可信结论。默认要求至少 10 个百分点跨度，并使用多点稳健拟合：

```text
used_percent(t) = intercept + 100 * cumulative_local_credit(t) / capacity + error
capacity = 100 / fitted_slope
```

共享账号默认污染；只有用户明确确认区间内没有其他机器使用，并且系统门禁全部满足，样本才可成为 `clean`。

系统只能把候选值“采纳为草稿”，最终 `confirmed` 必须由用户再次确认并保存。确认值新增版本，不覆盖历史。

证据面板同时显示内部 JSONL 计价与 ccusage 对账结果。只有比较区间能由完整日期或可精确对应的原始 Session 组成时才给出 ccusage 候选 Credit；无法精确切开 Reset 边界时显示“不可直接比较”，不得用近似 ccusage 数值替代内部事件级结果。

### 2.3 页面三：算法与数据口径

页面必须由实际配置和计算元数据生成，不复制一份容易过期的静态说明。内容包括：

- 指标定义和单位。
- Token 差分、缓存、总 Token、Credit、Fast、API 美元公式。
- 模型别名、长上下文阈值和价格版本。
- 日期、分钟、Session、对话组和 Reset 窗口边界。
- 容量拟合公式和样本排除条件。
- 数据源、数据血缘、质量标签和已知限制。
- ccusage 对账版本、最近结果和差异原因。
- JSONL/App Server 配额双源覆盖范围、冲突和来源选择原因。

每个页面指标都提供“如何计算”入口，返回原始来源、相关观察 ID、账号上下文、价格版本、计算版本和质量标记。

采集健康、历史映射、备份、导出和重算放在设置/诊断抽屉，不与口径文章混为一个信息层级。

## 3. 数据来源和职责

### 3.1 JSONL：本机生产事实源

目录：

```text
~/.codex/sessions/**/*.jsonl
~/.codex/archived_sessions/**/*.jsonl
```

白名单读取：

- `session_meta`
- `turn_context`
- `thread_settings_applied`
- `token_count`
- 与 Session 父子/fork 关系有关的结构化元数据
- `token_count` 内的结构化 rate-limit 投影

职责：

- 本机 Token 事件。
- 模型和模型切换。
- Standard/Fast 状态。
- Session、父子关系和 fork replay 证据。
- `token_count.rate_limits` 中的历史官方配额观察：primary/secondary `used_percent`、`window_minutes`、`resets_at`、limit、plan 和 credits。
- App Server 尚未运行或无法回溯时的历史 Reset 窗口和百分比趋势主证据。
- 分钟、Session、日期和本机窗口汇总的原始输入。

JSONL 采集器只写事实和游标，不直接计算日汇总、价格、容量或最终百分比。

JSONL 的历史配额采样是事件驱动的：只有 Codex 写入相关事件时才有观察点。它能恢复“观察到的历史趋势”和很多已记录的 Reset 信息，但不能凭空补出静默时段的分钟采样。页面必须显示观察点、carry-forward 和缺口，不能把稀疏历史插值伪装成官方连续曲线。

截至 2026-08-04 对本机真实历史做的只读结构检查（未读取或输出对话正文）确认：45 个 JSONL 文件包含 8,438 条 `token_count`，同样有 8,438 条 `rate_limits`；其中 4,357 条带 `used_percent/window_minutes/resets_at`，8,438 条带 `plan_type/credits`。因此历史配额回填是核心链路，同时必须接受“部分事件只有上下文、没有百分比窗口”的稀疏事实。

### 3.2 App Server：账号控制面和官方参考源

生产调用：

- `account/read`
- `account/rateLimits/read`
- `account/usage/read`
- `thread/list`
- 必要时不含 turns 的 `thread/read`

生产通知：

- `account/updated`
- `account/rateLimits/updated`
- `thread/tokenUsage/updated`
- Thread 标题/元数据相关通知

职责：

- 当前账号、认证方式、套餐和账号切换。
- 官方账号窗口百分比、Reset、limit ID 和 Credit 余额字段。
- 账号级每日 Token 参考 bucket。
- Thread 标题和可公开的结构化关系元数据。
- 与同一时段 JSONL rate-limit 观察交叉认证，发现字段漂移、采集缺口或口径冲突。

`account/rateLimits/updated` 是稀疏更新。缺失字段表示“本次未提供”，不能解释为清空。没有完整基线或合并冲突时必须重新执行 `account/rateLimits/read`。

### 3.3 ccusage：独立验证源

ccusage 不进入 `usage_deltas` 的生产写入链路，不决定账号归因，也不成为页面金额的唯一来源。它用于：

- JSONL Token 差分与去重结果验证。
- 日级和原始 Session 级 Token 对账。
- 模型别名、缓存口径、Fast 和长上下文计价参考。
- 固定 fixture 的黄金结果。
- 真实历史目录的只读 smoke test。
- 保存完整的日级和原始 Session 级验证结果，供页面人工逐项对比。

本轮计划审查参考的本地 ccusage 源码 commit 为 `5fd1591d3a4abdd63c0256b248157bf1568b57b8`；项目当前锁定和测试的 CLI 版本为 `20.0.19`。正式实施时需要把两者作为独立版本字段保存，因为“参考源码版本”和“实际执行的二进制版本”可能不同。

所有调用固定：

- 锁定版本。
- `--offline`。
- 明确 timezone、since/until、speed 和 pricing override。
- 保存命令参数摘要、版本、结果 hash、耗时和差异。
- 保存规范化结果行和经过白名单处理的原始 JSON 输出；历史运行不覆盖，使用 `superseded_by` 标记新一轮结果。

参考源码重点：

- `rust/adapters/codex/src/parser.rs`：累计差分、tier 继承、模型解析。
- `rust/adapters/codex/src/replay.rs`：fork/subagent 父关系和 replay prefix。
- `rust/adapters/codex/src/aggregate.rs`：事件级去重、日期/Session 聚合和长上下文 bucket。
- `rust/adapters/codex/src/report.rs`：非缓存输入、Fast 和长上下文计价。
- `rust/crates/ccusage-core/src/date_utils.rs`：IANA 时区和半开日期边界。
- `rust/crates/ccusage-core/src/pricing.rs`：模型别名、价格和长上下文阈值。

ccusage 同时承担两种验证：

1. 发布/升级门禁：对完整历史和固定 fixture 全量运行，差异未解释前不得发布。
2. 运行期可见对账：JSONL 发生变化后按退避策略重跑受影响日期和 Session，并允许用户从页面手工触发某一天、某个 Session 或完整历史的重新对账。

具体字段口径、`totalTokens` 对比规则、`costUSD` 的非权威含义和 ccusage 与 JSONL 的边界，已在主计划中固定；需要查原始调研证据时再打开来源参考。

### 3.4 JSONL 与 App Server 配额融合

两边都写入带 `source` 的原始 `quota_observations`，禁止采集阶段互相覆盖。规范化层再生成 canonical quota timeline：

- 历史上只有 JSONL：采用 JSONL 观察，质量为 `historical_jsonl`。
- 实时只有 App Server：采用 App Server 完整/合并快照，质量为 `live_app_server`。
- 同一时段两边都有：按账号、limit、window 和 reset generation 配对，逐字段比较。
- 值一致或在允许的采样/取整范围内：标记 `corroborated`。
- 值冲突：两份原始观察都保留，canonical 点按明确来源优先级选择，并标记 `quota_source_mismatch`。

当前实时状态优先使用 App Server 的完整读取；历史趋势优先使用时间更早、覆盖更广的 JSONL 观察。这里的“优先”只决定默认展示点，不删除另一来源，也不阻止用户查看两侧值。

历史 JSONL 通常只有 plan、provider、limit 等上下文，不一定能恢复稳定账号身份。只有与 App Server 重叠锚定、明确的本地身份证据或人工映射存在时，才能把历史窗口归到具体账号；否则保留 Unknown identity。

### 3.5 本地辅助文件：可选展示和归因补充

`state_5.sqlite`、`session_index.jsonl`、`config.toml` 和 `CODEX_HOME` 配置可以补充 Thread 标题、工作目录、模型设置、Session 索引、日志根目录和默认 provider，但不是 Token、配额或 Credit 主账本。是否启用这些文件必须在 R0 明确记录：

- 启用时只提取白名单字段，并保存 `source=local_auxiliary`；
- 禁用时页面使用 App Server/JSONL 可获得的标题和关系，缺失就显示 Unknown；
- 无论启用与否，都不能把这些文件中的配置推断成历史账号身份或官方窗口消耗。

字段清单和限制以主计划的数据模型为准，来源参考只作为实现时的补充索引。

## 4. 核心算法规范

### 4.1 Token 差分

每条 `token_count` 同时保留 `last_token_usage` 与 `total_token_usage`，不得在采集时丢弃其中之一。

每个原始 Session 独立维护上一份累计值：

1. 当前累计值与上一累计值完全相同：视为重复通知，不产生 delta，即使 `last_token_usage` 存在。
2. 累计值前进且存在 `last_token_usage`：优先使用 last 作为 delta。
3. 没有 last、存在当前累计值和上一累计值：逐字段做累计差。
4. 只有第一份累计值：可作为 `cumulative_first_observation`，但必须带来源质量；历史文件从头读取时通常可接受，半途接入时不得伪装精确。
5. 当前累计字段小于上一值：识别 counter reset/session rewrite，不使用普通饱和减法静默吞掉异常；开启新计数 epoch，并生成质量标记。
6. Token 全部为零：不产生计费用量事件。

缓存语义：

```text
non_cached_input = max(input_tokens - cached_input_tokens, 0)
comparable_total = non_cached_input + cached_input_tokens + output_tokens
```

`reasoning_output_tokens` 是输出子项，不重复加入总 Token 或计价。

### 4.2 模型和 service tier 继承

模型证据优先级：事件自身字段、`info`、`turn_context`、最近有效设置、带版本的 fallback 映射。原始模型名与规范化模型名都要保存。

service tier 规则：

- `default/standard` → Standard。
- `priority/fast` → Fast。
- 设置事件完全缺少 `service_tier` 字段：继承之前状态。
- 字段存在但值未知：清除旧状态，变成 Unknown。
- Unknown 不自动按 Standard 或 Fast 计价；正式值为空，同时给出 Standard–Fast 候选范围。

### 4.3 文件和事件去重

去重分三层，不能只靠一个 hash：

1. 文件层：path、inode、offset、mtime、最后完整行摘要，解决增量读取、移动和截断。
2. 原始事件层：`machine + session_id + event_kind + observed_at + normalized payload fingerprint`，解决 active/archived 副本和重放扫描。
3. fork replay 层：依据父子关系和父会话前缀显式抑制子会话复制的历史。

普通跨 Session 事件不能仅因时间、模型和 Token 相同就删除。只有存在明确父子/replay 证据时才做跨 Session 抑制，避免把真实并发请求误判为重复。

### 4.4 fork replay 去重

父关系来源：

- `session_meta.payload.forked_from_id`
- `session_meta.payload.source.subagent.thread_spawn.parent_thread_id`
- App Server 可确认的 Thread 关系
- 人工修订

父文件存在时：

- 读取父会话在 fork 时间之前的用量序列。
- 与子会话开头的用量序列逐项比较。
- 匹配的前缀标记为 `replayed_from_parent`，不进入生产用量。

父文件缺失时：

- 可以参考 ccusage 的“密集重写 burst”启发式，默认间隔阈值 1 秒。
- 启发式删除必须保存算法版本和证据，并标记 `replay_heuristic`。
- `replay_heuristic` 数据不得进入高可信容量标定，除非通过 ccusage 对账和人工确认。

### 4.5 Session、父子关系和展示合并

系统分开保存：

- 原始 Session：Codex 的真实 `session_id`，是事实和对账单位。
- 对话关系：parent/root/relation 构成的有向关系。
- 对话组：面向页面，把主 Session、子代理和 fork 后续用量归到一个 root 下的派生视图。

关系类型至少包括：

- `root`
- `fork`
- `subagent`
- `review`
- `unknown_child`
- `manual`

root 解析规则：

1. 优先显式父 ID。
2. 防止 self-parent 和环；出现环时不合并并标记错误。
3. 父缺失时子 Session 自成 orphan root，同时保留 parent ID。
4. 人工修订新增版本，不改写原始关系。

计费永远按去重后的事件一次计入。对话组只是另一种汇总视图，不能再次把父子 totals 相加。

### 4.6 时间、日期和边界

所有事实时间保存 UTC epoch 毫秒。日期和周显示使用 `machines.timezone` 的 IANA 时区。

本地日期 `D` 的严格边界：

```text
[start_of_day(D, timezone), start_of_day(D + 1, timezone))
```

不能假设一天固定 24 小时；使用 IANA timezone 库处理 DST。事件按自身时间归日，跨午夜 Session 拆入两个 `daily_session_rollup`，不按 Session 开始日期整体归入一天。

价格按事件时间选择：

```text
effective_from <= event_at < effective_to
```

账号上下文也使用半开区间。事件恰好落在切换边界时归入新区间；缺少足够证据的边界事件标记为 `mixed_account/ambiguous_boundary`。

### 4.7 分钟聚合

分钟主键使用 UTC epoch minute，不使用本地时间字符串，避免 DST 重复分钟冲突。显示时再转成本地时间。

分钟 Token/Credit 是该分钟内 delta 之和。官方百分比保存真实观察点；用于画阶梯线的 carry-forward 是查询层行为，并返回真实观察时间和 stale 状态。

### 4.8 配额窗口和日内百分比

窗口身份至少包含：

```text
account_identity + limit_id + window_minutes + reset_generation
```

以下任一情况开启新窗口：

- `resets_at` 跨过当前时间并出现重置后样本。
- `resets_at` 发生合理的新一代变化。
- 同一窗口内 `usedPercent` 明显下降且排除稀疏更新/乱序样本。
- 账号或 limit 变化。

一天内发生 Reset 时，当日百分比消耗按窗口段分别计算再相加，不能简单使用日末减日初。日初/日末 carry-forward 只有在最近真实样本未超过 staleness 阈值时才可用。

### 4.9 历史配额回填与双源认证

历史回填按全局两遍以上处理，不能逐文件读到一条就立即形成最终窗口：

1. 扫描 active/archived 全部文件，建立文件身份、Session 元数据和父子关系。
2. 建立完整 replay plan，随后生成 replay-safe Token delta。
3. 独立抽取全部 JSONL rate-limit 观察，保留 primary/secondary、limit、plan、credits 和来源事件。
4. 按观察时间、limit 和 `resets_at` 重建历史 reset generations；计划 Reset 时间可给出边界，实际重置确认仍依赖重置前后观察。
5. 使用 App Server 重叠时段做身份锚定和配额交叉认证。
6. 在账号身份、价格、日期和 Reset 半开边界上切分 usage delta。
7. 生成分钟、日期、Session、窗口投影和 ccusage 对账。

历史趋势分三种精度：

- `observed`：JSONL/App Server 的真实采样点。
- `scheduled_boundary`：来自上一观察的 `resets_at`，但缺少重置后近邻样本。
- `inferred_gap`：只能确定 Reset/变化发生在两次观察之间。

页面用实心点、边界线和阴影缺口分别表达，禁止把后两类画成连续精确采样。

### 4.10 计价

订阅 Credit 与 API 等价美元使用两套完全独立的价格版本和 Fast 倍率。价格以“每百万 Token 的整数微单位”保存，计算使用整数或有界十进制定点，不以 SQLite `REAL` 作为正式账本。

普通请求：

```text
base = non_cached_input * input_rate
     + cached_input * cache_rate
     + output * output_rate
```

Fast：

```text
total = base * fast_multiplier
fast_surcharge = total - base
```

长上下文必须按每次请求判断，不能先汇总一天 Token 再判断。对于 OpenAI 这类“请求输入超过阈值后整次请求切换价格”的模型，输入、缓存和输出 bucket 全部使用该请求的长上下文费率。阈值和长上下文价格属于模型价格版本。

模型价格缺失时金额为 null，并标记 `missing_pricing`，绝不按 0 展示为正式结果。

### 4.11 账号每日 Token 参考

`account/usage/read` 的 bucket 规范化保存：原始 `startDate`、tokens、读取时间、账号、原始快照 ID、对齐时区和 freshness。

只有同一账号、已结算、日期可比较且本机无 API 混杂时计算：

```text
signed_difference = account_tokens - local_tokens
unobserved_tokens = max(signed_difference, 0)
coverage_ratio = local_tokens / account_tokens
```

数据库保留 signed difference；页面主视图显示非负未观测量，并在本机大于账号值时明确提示口径或结算异常。该维度不参与 Credit、窗口占比或容量确认。

### 4.12 容量估计

候选样本必须满足：

- 同一账号、同一 limit、同一 Reset 窗口。
- 同一可解释的价格版本，或已按事件精确拆分。
- 无账号切换和不可拆分 delta。
- 配额跨度至少 10 个百分点。
- 无关键采样缺口。
- 用户确认区间只有本机使用，或有其他独立证据支持。
- 不含 `missing_pricing`、`fast_unknown`、`replay_heuristic` 等阻断标签。

官方百分比若为整数，单个观察值表示量化区间而非精确点。拟合输出必须包含容量点估计、量化误差区间、稳健离散程度、样本数和污染说明。

### 4.13 ccusage 对账

对账维度：

- 原始 Session 总 Token及分类。
- 本地日期总 Token及分类。
- 模型拆分。
- Standard 强制计价。
- 自动 Fast 计价。

对账前必须对齐：ccusage 版本、timezone、since/until、模型别名、pricing override、speed policy 和输入目录。

每次运行保存：

- run ID、触发原因、范围、命令参数、ccusage 版本和源码参考版本。
- `daily` 和 `sessions` 的规范化结果行。
- Token 分类、模型拆分、cost、lastActivity 等白名单字段。
- 原始结果 hash、白名单原始 JSON、退出状态、耗时和 stderr 脱敏摘要。
- 与同范围内部结果的逐指标 comparison。

页面默认展示最近一次成功且参数兼容的结果；旧运行保留用于观察升级前后差异。参数不一致的两次结果不得被标记为可直接比较。

门禁：

- 固定 fixture Token 差异必须为 0。
- 固定 fixture 模型拆分差异必须为 0。
- 定点金额转换前与 ccusage 浮点输出的差异不得超过明确的舍入容差。
- 真实历史差异若不为 0，必须生成可分类差异，不允许只记录“对不上”。

## 5. 数据库最终结构

### 5.1 原始事实层

| 表 | 作用 |
| --- | --- |
| `machines` | 机器、安装 ID、IANA 时区 |
| `account_identities` | 脱敏账号身份 |
| `account_context_intervals` | 半开账号/provider/套餐上下文 |
| `jsonl_files` | 增量读取游标和文件身份 |
| `jsonl_observations` | 白名单原始事件投影、原始 fingerprint |
| `token_observations` | last 与 cumulative Token 两套原始证据 |
| `thread_setting_observations` | 模型和 service tier 变化 |
| `quota_observations` | JSONL/App Server 两个来源的原始配额观察 |
| `quota_snapshots` | 规范化 canonical 配额时间线和来源选择 |
| `quota_raw_notifications` | App Server 稀疏通知审计 |
| `quota_source_comparisons` | JSONL/App Server 同期逐字段认证结果 |
| `account_usage_snapshots` | 账号 usage 原始读取元数据 |
| `account_usage_daily_buckets` | 规范化账号日 Token bucket |
| `sessions` | 原始 Session 和标题元数据 |
| `session_relations` | parent/root/relation 和证据 |

### 5.2 业务事实层

| 表 | 作用 |
| --- | --- |
| `usage_deltas` | 去重、归因、计价后的最小本机事实 |
| `usage_delta_sources` | delta 到原始观察的多对多血缘 |
| `quota_windows` | Reset 窗口身份与边界 |
| `pricing_versions` | 不可变价格版本 |
| `pricing_rates` | 模型、scheme、tier、长上下文价格 |
| `capacity_versions` | 20/100/200 档人工版本 |

### 5.3 投影层

| 表 | 作用 |
| --- | --- |
| `minute_rollups` | 分钟 Token、Credit、美元 |
| `session_rollups` | 原始 Session 汇总 |
| `conversation_rollups` | 对话组汇总 |
| `daily_rollups` | 日期与账号上下文汇总 |
| `daily_session_rollups` | 跨日 Session 的日内份额 |
| `window_rollups` | Reset 窗口本机累计 |
| `token_reference_daily` | 账号/本机日 Token 参考对账 |
| `calibration_segments` | 用户选择的标定证据段 |
| `calibration_results` | 候选值、区间和拟合诊断 |

### 5.4 质量、对账和审计

| 表 | 作用 |
| --- | --- |
| `quality_flags` | 一条结果可关联多个质量问题 |
| `reconciliation_runs` | ccusage 调用上下文和版本 |
| `ccusage_daily_results` | 持久化 ccusage 日级结果和模型拆分 |
| `ccusage_session_results` | 持久化 ccusage 原始 Session 结果和模型拆分 |
| `ccusage_raw_outputs` | 白名单原始 JSON、hash 和 supersede 关系 |
| `reconciliation_differences` | 指标级差异和原因 |
| `collector_runs` | 采集健康、耗时、错误摘要 |
| `calculation_runs` | 计算版本、输入水位和重建状态 |
| `manual_annotations` | 人工关系、账号和容量修订审计 |

所有派生表必须可删除并从事实层重建。事实表只追加或以明确 supersede 关系修订，不由页面直接覆盖。

## 6. 后端模块和运行架构

```text
src/
  main.rs
  config.rs
  domain/
  storage/
  collectors/
    jsonl/
    app_server/
    ccusage_reconciler/
  normalization/
    token_delta.rs
    model.rs
    service_tier.rs
    replay.rs
    dedupe.rs
    session_graph.rs
  attribution/
  pricing/
  windows/
  rollups/
  calibration/
  reconciliation/
  api/
  scheduler.rs
```

单一后台进程：

- 管理 App Server 子进程、通知和重连。
- 监听 JSONL，增量写事实。
- 串行化同一 Session 的 normalize/project 工作，跨 Session 可并行。
- 维护 dirty range 队列，只重建受影响的分钟、日期、Session 和窗口。
- 低频执行 ccusage 对账，不阻塞 JSONL 生产账本。
- 同源提供本地 API 和静态前端。

## 7. API 契约先行

实现采集器扩展前，先冻结以下只读契约：

```text
GET /api/v1/overview
GET /api/v1/days?from=&to=&account=
GET /api/v1/days/{date}
GET /api/v1/days/{date}/minutes
GET /api/v1/days/{date}/sessions?grouping=raw|conversation
GET /api/v1/windows?account=&limit=
GET /api/v1/windows/{id}
GET /api/v1/calibrations
GET /api/v1/reconciliation/days/{date}
GET /api/v1/reconciliation/sessions/{session_id}
GET /api/v1/reconciliation/quota?from=&to=
GET /api/v1/reconciliation/runs
GET /api/v1/methodology
GET /api/v1/health
```

写接口：

```text
POST /api/v1/calibration-segments
POST /api/v1/capacity-versions
POST /api/v1/manual-annotations
POST /api/v1/rebuilds
POST /api/v1/reconciliation-runs
POST /api/v1/exports
```

所有指标对象统一返回：

```text
raw_value
display_value
unit
freshness
quality_flags[]
source_summary
calculation_version
pricing_version
```

## 8. 从当前代码迁移的原则

### 8.1 保留

- Git 历史和现有三个提交。
- fixture、脱敏脚本和 App Server schema 快照。
- SQLite 连接、WAL、外键和 identity HMAC。
- JSONL 文件发现、游标、watcher、半行、截断、归档和幂等测试。
- auth kind、plan type、display group、capacity profile 分离原则。

### 8.2 重构

- `token_observations` 同时保存 last/cumulative 证据。
- `Quality` 从单枚举变成 summary severity + 多 flag。
- Credit/USD 从 `REAL` 改为整数微单位。
- account daily bucket 从 JSON blob 规范化。
- 日汇总从 JSONL collector 移到 rollup projector。
- ccusage 从生产计价快照改成 reconciliation。
- 新增 Session 图、fork replay 和 conversation group。

### 8.3 schema 策略

仓库尚未发布且没有正式数据库文件时，允许进行一次 baseline 重整：更新初始 migration 和测试库，从空库重建。保留旧设计于 Git 历史，不保留无用表作为永久包袱。

如果实施前发现工作区外已有必须保留的数据库，则停止 baseline 重整，改为新增迁移并编写一次性数据转换和回滚前备份。两种策略不能混用。

## 9. 分阶段实施和门禁

### 阶段 R0：工具链和设计冻结

任务：

- 修复 Rust 工具链缺少 `libLLVM.dylib` 的问题。
- 保存当前未提交文档，不覆盖用户改动。
- 冻结指标字典、三页线框、API 示例和数据血缘矩阵。
- 固定 ccusage 版本和源码参考 commit。
- 明确是否存在需迁移的外部数据库。

门禁：

- `cargo test`、web test 和 fixture 校验全部可运行。
- 每个页面指标都能映射到数据源和算法。
- schema 采用 baseline 或 migration 的决策有书面记录。

### 阶段 R1：事实模型重整

任务：

- 重整 schema 和领域类型。
- 建立多质量标签、整数计价单位和数据血缘。
- 增加 sessions、relations、normalized account buckets。
- 删除采集器到日汇总的直接依赖。

门禁：

- 空库可重复建库。
- 事实表可写，派生表可全部删除重建。
- 不存在用 0 代表未知金额或缺失 bucket 的路径。
- 数据库不含对话正文和认证秘密。

### 阶段 R2：JSONL 正规化引擎

任务：

- 重构 token last/cumulative 处理。
- 实现模型、tier、事件去重和 counter epoch。
- 实现 fork replay plan 和 Session 图。
- 完整抽取历史 primary/secondary 配额、Reset、limit、plan 和 credits 观察。
- 实现可断点、可重跑的全历史 backfill，先建立全局 Session/replay 关系再生成最终 delta 和窗口证据。
- 将现有增量读取接入新事实模型。

门禁：

- 重复累计通知不计数。
- last 缺失时累计差分正确。
- counter 回退不会静默丢失或制造负数。
- active/archived、归档移动、半行、截断无重复。
- parent 可用和缺失两类 fork replay fixture 通过。
- 并发真实相同 Token 事件不会被错误跨 Session 删除。
- 历史 fixture 的 `used_percent/window_minutes/resets_at` 原样保存，重跑不重复。
- 没有 App Server 的历史日期仍能生成带缺口的 quota observation 趋势。

### 阶段 R3：内部 Token 对账

任务：

- 生成去重后的原始 Session 和日 Token 汇总。
- 建立 ccusage `daily/session --json` 固定调用。
- 持久化每次 ccusage run、日级结果、Session 结果、白名单原始 JSON 和逐指标 difference。
- 提供按日期、Session、受影响范围和完整历史的重跑入口。

门禁：

- fixture 的总 Token、非缓存输入、缓存、输出和模型拆分与 ccusage 完全一致。
- fork、跨日、Fast 切换和历史副本 fixture 对账一致。
- 真实目录 smoke test 差异可解释。
- 同一范围的内部/ccusage 两侧结果可以被 API 直接读取；旧版本运行不被覆盖。

### 阶段 R4：App Server 控制面

任务：

- 管理子进程、初始化、通知、重连和版本兼容。
- 采集账号、配额、Reset、账号日 Token 和 Thread 元数据。
- 实现完整快照 + 稀疏通知 merge。
- 将 App Server 配额观察与 JSONL 重叠历史逐字段认证，生成 corroborated/mismatch。
- 账号变化时原子关闭旧上下文并开启新上下文。

频率：

- `account/read`、`account/rateLimits/read`：启动、重连、账号变化立即读取。
- 配额兜底：活跃 60 秒、空闲 5 分钟。
- Reset：`T-60s`、`T+15s`、`T+60s`。
- `account/usage/read`：启动/重连、每 6 小时、跨日后。
- Thread 元数据：启动分页同步，通知或 JSONL 新 Session 时增量刷新。

门禁：

- 稀疏通知不清空有效字段。
- 当前日 bucket 缺失为 pending，不写 0。
- 账号切换区间不重叠。
- App Server 断线不阻塞 JSONL；恢复后自动补快照。
- Thread 读取不请求或保存 turns 正文。
- JSONL/App Server 冲突时两份原始数据都保留，页面可查看来源选择原因。

### 阶段 R5：归因和计价引擎

任务：

- 生成最小 `usage_delta`。
- 按事件时间连接账号上下文和价格版本。
- 实现订阅 Credit/API 美元两套定点计价。
- 实现 Fast、Unknown 范围和长上下文请求级计价。
- 建立 ccusage 计价对账。

门禁：

- 边界前后 1ms 选择正确版本。
- 同一事件只归一个账号上下文。
- 两套 scheme 和 Fast 倍率不可互换。
- 未知模型金额为 null。
- fixture Standard/Auto Fast 与 ccusage 在容差内一致。
- 历史价格新增不改变旧 delta 的 version ID。

### 阶段 R6：分钟、Session、日期投影

任务：

- 实现 dirty range 和增量 projector。
- 生成 minute/session/conversation/day/daily-session rollup。
- 实现本地日期半开边界和跨日 Session 拆分。
- 建立可重复的全量 rebuild。

门禁：

- 所有粒度求和回到同一组 usage delta 总数。
- 跨午夜、DST、账号切换和价格切换 fixture 通过。
- 增量投影结果与全量重建完全一致。
- JSONL 单次新增不扫描并重算全部历史。

### 阶段 R7：配额窗口和 Token 参考

任务：

- 建立 quota window、Reset 识别和 carry-forward 新鲜度。
- 使用 JSONL 历史观察回填 App Server 启动之前的百分比和 Reset 趋势。
- 生成 window rollup 和本机累计占比。
- 生成 normalized account/local daily Token 参考。
- 计算 signed difference、unobserved 和 coverage。

门禁：

- Reset 不跨窗口连线。
- 一天内多窗口百分比按段计算。
- stale 样本不参与正式首尾百分比。
- Token 参考不影响 Credit 和容量。
- 身份不一致、API 混杂和远程 pending 时不可比较。
- historical_jsonl、live_app_server、corroborated 和 mismatch 在 API 中可区分。

### 阶段 R8：只读 API 和页面一

任务：

- 实现 overview、day、minute、session、window、methodology、health API。
- 构建用量总览完整页面。
- 在日详情和原始 Session 中并排展示内部 JSONL 结果、ccusage 结果与差异。
- 在配额趋势中显示 JSONL/App Server 来源、重叠认证状态和历史缺口。
- 接入指标血缘和质量说明。

门禁：

- Plus 单机、Pro 共享、API、Unknown、跨账号日、Reset、缺样和空状态均有 UI 测试。
- 日历、分钟和 Session 汇总能追溯到同一事实。
- 1440×900、1920×1080 和窄窗口无溢出。
- 页面不把账号总量标成个人本机量。
- 用户可以手工重跑选中日期/Session 的 ccusage 对账并看到新旧运行。

### 阶段 R9：容量标定引擎和页面二

任务：

- 实现窗口/区间选择、清洁度判定、候选公式和稳健拟合。
- 实现量化误差区间和污染诊断。
- 实现 capacity draft/confirmed/retired 版本。
- 构建容量标定页面。

门禁：

- 少于 10 个百分点默认不能标为高可信。
- 共享账号未经确认不得自动成为 clean。
- 阻断质量标签不能进入正式拟合。
- 候选不能自动覆盖 confirmed。
- 使用确认容量重算页面占比不改写历史 Credit。

### 阶段 R10：页面三、设置、安全和运维

任务：

- 构建算法与数据口径页。
- 构建设置/诊断、导出、备份和重算。
- 实现 Origin、CSRF、无通配 CORS 和 loopback 限制。
- 实现 launchd、日志轮转和健康检查。

门禁：

- 非法 Origin 和无 CSRF 的写请求被拒绝。
- 导出不含秘密和对话正文。
- 每个主要指标能展示算法和版本。
- 重启后恢复游标、App Server 和 dirty projector。

### 阶段 R11：真实运行验收

任务：

- 连续运行至少 7 天。
- 首次上线前对完整 JSONL 历史执行 backfill 和 ccusage 全量对账。
- 验收 Plus 单机、Pro 共享、账号切换、Fast 切换、一次 Reset。
- 验收历史已结算日账号/本机 Token 对账。
- 记录 CPU、内存、数据库增长、扫描耗时和重连情况。

门禁：

- 七天无重复计数和不可解释总量漂移。
- 空闲 CPU 接近 0，数据库增长可解释。
- ccusage Token 对账为 0 或全部差异已分类。
- 历史 JSONL 配额趋势可见，和 App Server 重叠部分有认证结果。
- 页面三个核心场景通过人工验收。

## 10. 测试矩阵

至少覆盖：

- JSONL：重复累计、last 缺失、counter reset、半行、截断、移动、副本、乱序时间。
- Session：root、fork、subagent、多级子代理、父缺失、自环、环、人工修订。
- replay：完全匹配前缀、父 fork 后新增、重写 burst、并发相同事件。
- tier：Standard→Fast→Standard、字段缺失继承、未知值清除。
- 模型：事件模型、turn context、fallback、别名、未知模型。
- 时间：北京时间午夜、UTC 午夜、DST 23/25 小时日、价格边界前后 1ms。
- 配额：完整快照、稀疏 merge、乱序样本、Reset、同日多 Reset、stale carry-forward。
- 配额双源：仅 JSONL 历史、仅 App Server 实时、双源一致、取整差、双源冲突、身份无法锚定。
- 账号：Plus、Pro、API、未知、切换边界、跨账号 Session。
- 计价：缓存、输出、reasoning 子项、Fast、长上下文、两套 scheme、缺价格。
- 对账：daily/session、模型拆分、日期过滤和 rounding tolerance。
- 对账持久化：运行版本、参数兼容、失败保留旧成功结果、手工重跑和旧结果 supersede。
- 容量：干净区间、共享污染、整数百分比量化、缺样、异常点。
- API/UI：所有质量状态、空状态、刷新和重算中状态。

## 11. 提交和执行纪律

建议提交顺序：

```text
docs: freeze product metrics and v2 execution plan
fix(dev): restore reproducible rust toolchain
refactor(storage): establish canonical usage facts
refactor(jsonl): normalize codex token deltas
feat(sessions): model replay-safe conversation graphs
feat(reconcile): verify jsonl totals with ccusage
feat(app-server): collect account quotas and thread metadata
feat(pricing): calculate versioned credit and usd usage
feat(rollups): project minute session day and window usage
feat(api): expose traceable usage contracts
feat(web): build local usage dashboard
feat(calibration): estimate and confirm weekly capacity
feat(web): document metrics and data lineage
feat(service): secure and operate the local daemon
```

每个提交都必须可独立测试和回退。阶段门禁未通过时不得提前开始依赖该阶段正确性的后续页面。

## 12. 第一版完成定义

第一版完成必须同时满足：

1. 用量页能按日期、分钟、原始 Session 和对话组展示本机 Token、Credit、API 美元和账号窗口变化。
2. 页面明确区分本机、账号、未观测和参考指标。
3. JSONL Token 在固定 fixture 上与 ccusage 日级、Session 级完全一致。
4. ccusage 日级、Session 级结果和逐项差异被持久化，并可在页面人工对比和重跑。
5. JSONL 能回填 App Server 启动前的历史官方百分比和 Reset 观察；重叠区间与 App Server 有可见认证结果。
6. fork/subagent replay 不重复收费，真实子会话后续用量不被删除。
7. 模型、Fast、长上下文和历史价格按事件级正确计价。
8. Reset 窗口、日期、账号和价格边界均使用半开区间且有边界测试。
9. 容量页能生成带误差、污染和样本证据的候选值，并只能人工确认。
10. 算法页能解释主要指标的来源、公式和版本。
11. App Server 或 ccusage 暂时失败不会破坏 JSONL 本机事实账本。
12. 服务连续运行七天，无重复、失控扫描、持续高 CPU 或不可解释漂移。
13. 数据库不保存对话正文或认证秘密，本地写接口通过安全门禁。
