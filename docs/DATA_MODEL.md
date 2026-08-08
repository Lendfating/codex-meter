# Codex Meter 页面指标与八表数据模型

> 状态：2026-08-08 最终稿（对应当前代码 `config/schema.sql` 与 `src/db.rs`）
>
> 作用：固定三个页面到底显示什么、每个指标来自哪里、八张表必须保存什么。后续实现不得自行增加表或无页面消费者的字段。
>
> 优先级：本文件是页面数据契约；若与旧设计的多层数据库冲突，以当前用户要求和本文件为准。

## 1. 最终结论

数据库只保留八张表：

1. `source_jsonl`：JSONL 白名单事实。
2. `source_app_server`：App Server 账号、配额、账号日 Token 快照。
3. `source_ccusage`：ccusage 日级和 Session 级校验结果。
4. `usage_daily`：日期粒度结果。
5. `usage_minute`：分钟粒度结果，也是 Reset 窗口的最小时间轴。
6. `usage_window`：物化的 Reset/周窗口结果，避免页面每次从分钟重算。
7. `usage_session`：Turn 粒度结果，通过 `root_session_id` 聚合成页面 Session。
8. `capacities`：人工确认的 20/100/200 美元档周 Credit 容量。

> 兼容说明：当前新 baseline 直接使用 `capacities`；旧归档表不属于新运行链。

不建立模型表、当前窗口表、价格表、设置表、文件表、质量表或校验差异表：

- 模型汇总从 `usage_session` 聚合。
- Reset 窗口结果物化到 `usage_window`，分钟仍是详细时间轴。
- 当前窗口从最新官方快照和当前 `window_id` 聚合。
- JSONL/ccusage 差异在查询时按日期或 Session 关联。
- 价格和公式放在版本化代码配置中。
- JSONL 文件游标放在可删除、可重建的 sidecar 状态文件中。
- 质量只使用各行的一个紧凑 `quality` 字段。

原始 JSONL 继续留在 Codex 自己的目录。数据库不复制原始行、对话正文、提示词、回复、推理、工具输入输出、认证秘密或完整 App Server/ccusage 响应。

## 2. 页面数据契约

### 2.1 全局框架

- 左侧默认折叠菜单：用量报告、容量估算、计算说明。
- 页面左上显示 Codex Meter 品牌和当前页标题。
- 用量报告右上显示当前订阅，例如 `Plus（20 美元）`、provider、官方数据是否可用。
- 所有缺失值显示“待补数据/未观测/不可比较”，不显示伪造的 `0`。
- Token 使用 `K/M/G`，美元保留两位，Credit 使用必要精度，百分比通常保留一位。

### 2.2 页面一：用量报告

#### A. 当前窗口

| 指标 | 页面含义 | 来源/计算 |
| --- | --- | --- |
| 当前账号/认证类型 | ChatGPT、API Key、Bedrock 或未知 | 最新 `source_app_server(kind=account)` |
| 当前套餐/provider | Plus、Pro、Other/API 等 | App Server 当前值；历史按 JSONL 证据 |
| 当前套餐周额度 | 人工确认的周 Credit 总量 | `capacities` 当前有效记录 |
| 官方已用/剩余 | 账号周窗口状态 | 当前优先 App Server，历史优先 JSONL |
| 窗口开始/下次 Reset | 当前 Reset 区间 | 配额快照推导的 `window_id/start/reset` |
| 已经过/距 Reset | 时间差 | 当前时间与窗口起止时间 |
| 账号 Token（窗口参考） | 从窗口起始日到昨天的账号日 Token 合计 | `usage_daily.account_tokens`；首日边界近似，今天可能缺失 |
| 本机 Token | 当前窗口内本机 Token | 当前 `window_id` 的 `usage_minute` 合计 |
| 本机 API 美元 | 当前窗口内 API 等价美元 | 当前 `window_id` 的 `usage_minute` 合计 |
| 本机 Credit | 当前窗口内订阅 Credit | 当前 `window_id` 的 `usage_minute` 合计 |
| 本机周占比 | 本机 Credit / 当前套餐周额度 | 容量未确认时为 `NULL` |

账号 Token 是延迟的日级参考，不和本机实时值伪装成相同精度。API 模式不计算订阅 Credit 或本机周占比。

#### B. 按日期总览

左侧最近 30 天日历，每格显示：

- 日期；
- `T 本机 Token / 账号 Token`；
- `API 美元 · Credit`；
- `Δ% 本机日占比 / 账号官方日变化`。

日历颜色按本机实际花费强度：订阅日优先 Credit，API 日优先 API 美元；不按 Token 大小着色。点击日历或右侧图表中的日期时，两边同步高亮，下面详情同步切换。

右侧图表的指标顺序固定为：

1. Token：本机每日 Token、账号每日 Token；可选展示本机输入/缓存读/缓存写/输出分类。
2. API 美元：本机每日 API 等价美元。
3. Credit：本机每日订阅 Credit。
4. 周窗口 %：
   - 柱状：当天本机占比变化、当天账号官方已用变化；
   - 折线：官方剩余、官方从最近 Reset 累计已用、本机从最近 Reset 累计占比；
   - Reset 前后不连线。

Token、API 美元和 Credit 的日图表示“当天消耗”，不是跨日累计。

#### C. 选中日期的双来源对比

JSONL 主事实与 ccusage 校验按同一列上下对齐：

- 非缓存输入；
- Cache Read；
- Cache Write（来源没有时为 `NULL`）；
- 输出；
- 推理输出（是输出子集，不重复计总量）；
- 总 Token；
- 订阅 Credit；
- API 等价美元；
- 差值由接口即时计算，不单独落表。

账号日 Token 另列为参考：账号 Token、本机 Token、未观测 Token、覆盖率、`pending/stale/settled/incomparable`。它不参与 Credit 或容量计算。

#### D. Session 与 Turn

页面 Session 列表按根对话合并 child/fork，显示：

- 对话标题；
- 开始/结束时间；
- main/child/fork 关系和成员数量；
- 主模型/全部模型；
- Fast/Standard/mixed/unknown；
- Token、Credit、API 美元。

展开一个 Session 后按 Turn 显示：

- Turn ID；
- 来源 Session ID 和 main/child/fork 关系；
- 开始/结束时间；
- 模型和 Fast 状态；
- Token 分类、总 Token、Credit、API 美元；
- 有真实采样时显示官方百分比开始/结束；没有时保持空值。

Turn 内发生多模型或 Fast 切换时，不拆成新数据库表；使用该 Turn 的紧凑 `model_breakdown_json` 保存实际模型/tier 用量段。

#### E. 分钟级变化

指标顺序仍为 Token、API 美元、Credit、周窗口 %：

- Token/API 美元/Credit 从当天 00:00 的 0 开始累计。
- 账号 Token 只有日级值；分钟图中只能画日级参考线或标注，不伪造分钟斜率。
- 周窗口百分比显示官方剩余、官方累计已用、本机累计占比。
- 当天发生 Reset 时，官方剩余发生跳变；官方累计和本机累计在 Reset 点清零重算。
- 分钟明细表默认折叠。

`usage_minute` 对“有本机用量的分钟”或“有官方配额观测的分钟”各保留一行，因此即使本机空闲，也能保存必要的官方窗口变化。

#### F. 按周窗口

按日期和按周窗口使用同一布局，区别只有粒度。一个周窗口是同一账号、limit 和 Reset 区间：

- 左侧每个卡片显示窗口开始、Reset、持续时间、本机/账号 Token、本机 API 美元/Credit、本机/官方百分比变化、共享状态。
- 右侧一个点代表一个 Reset 窗口，可切换 Token/API 美元/Credit/周窗口 %。
- 下方仍显示 JSONL/ccusage 对比、窗口内 Session 和窗口内分钟/小时变化。

Reset 窗口卡片和趋势直接读取 `usage_window`；分钟明细仍从 `usage_minute` 读取。

### 2.3 页面二：容量估算

- 默认时间范围最近 30 天，使用双端点滑块。
- 总图可跨多个 Reset 窗口浏览；真正用于估算的选中区间必须在同一个 `window_id` 内。
- 图表左轴是百分比，显示官方剩余、官方累计已用、本机累计周占比。
- 图表右轴是当前 Reset 起点以来的本机累计 Credit。
- Reset 位置画垂线，累计线在 Reset 处清零。
- 选中区间后显示：本机 Credit 增量、官方百分比增量、本机占比增量、候选周容量。
- 自动估算只在同一 Reset 内扫描有效滑动区间；官方变化不足最小阈值（当前 5%）的样本不使用。
- 自动结果显示最大有效候选、有效区间数量和证据区间；它只给建议，不自动写入确认容量。
- 详细日表和 Reset 窗口表默认折叠。
- 页面底部仅保留 20/100/200 美元档三个人工确认输入。

候选公式：

```text
候选周容量 = 本机区间 Credit / (官方 usedPercent 增量 / 100)
```

共享账号或存在未观测消耗时标记污染，不把候选值自动当作真实容量。

### 2.4 页面三：计算说明

只显示：

- Token 总量、缓存读/写、推理输出的口径；
- last/cumulative 差分、去重、counter reset、fork replay 处理；
- 订阅 Credit、API 等价美元、Fast 附加量公式；
- 当前代码价格配置和生效时间；
- JSONL、App Server、ccusage 的职责；
- 人工容量与本机占比公式；
- 当前质量状态及缺失原因。

价格不存数据库，放在随代码发布的版本化静态配置中。历史事件按事件时间选择配置版本。

## 3. 核心指标口径

```text
total_tokens = non_cached_input + cache_read + output
```

- Cache Write 单独展示和计价，但不重复加入已经包含它的总量字段。
- reasoning output 是 output 子集，单独展示但不重复计总量或价格。
- 本机 Credit/API 美元按每个 Token 增量当时的模型、tier 和价格版本计算。
- 本机日占比使用“当天有效的 capacity_profile”对应容量作分母；套餐在日期内切换且无法切开时，日占比为 `NULL/mixed_account`。
- 官方百分比是账号级；本机百分比是本机 Credit / 人工容量。两者不能互相覆盖。
- 账号日 Token 是延迟参考；当前日期缺失不填 0。

## 4. 八张表的最终结构

所有时间使用 UTC epoch 毫秒；页面固定按 `Asia/Shanghai` 切日。时区作为代码常量，不另建设置表。

### 4.1 `source_jsonl`

该表只保存四类白名单语义记录：`session`、`turn`、`usage`、`quota`。不保存其他 JSONL 行。

| 字段 | 用途 |
| --- | --- |
| `id` | SQLite 主键 |
| `source_key` | 唯一摘要；负责 active/archived、重读和重复通知去重 |
| `kind` | `session/turn/usage/quota` |
| `observed_at_ms` | usage/turn 实际时间；quota 状态首次观察时间 |
| `last_seen_at_ms` | quota 相同状态最后确认时间；其他 kind 可为空 |
| `session_id` | 实际 Session/Thread ID |
| `parent_session_id` | 直接父 Session；用于第二批沿父链合并 child/fork |
| `root_session_id` | 第一批沿完整 `parent_session_id` 链解析出的最终根对话；无法解析时为 `NULL` 并标记质量 |
| `turn_id` | Turn ID；缺失保持 `NULL` |
| `relation` | `main/child/fork/unknown` |
| `title` | 仅 session 行使用；无可靠来源时为 `NULL` |
| `started_at_ms` / `ended_at_ms` | turn 行起止；活动中 `ended_at_ms=NULL` |
| `model` | usage 时实际模型或 turn 主模型 |
| `service_tier` | `fast/standard/unknown` |
| `reasoning_effort` | `low/medium/high/...`；来源缺失时为 `NULL` |
| `provider` | 原始 provider |
| `plan_type` | `plus/pro/.../NULL` |
| `input_tokens` | 非缓存输入增量；仅 usage 行 |
| `cache_read_tokens` | Cache Read 增量 |
| `cache_write_tokens` | Cache Write 增量 |
| `output_tokens` | 输出增量 |
| `reasoning_tokens` | 推理输出增量，属于输出子集 |
| `total_tokens` | 已归一化总增量 |
| `limit_id` / `window_kind` | quota 行的配额桶和 primary/secondary |
| `used_percent` | quota 行官方已用百分比 |
| `window_minutes` | 官方窗口时长 |
| `resets_at_ms` | 官方下次 Reset |
| `quality` | 紧凑质量字符串；无独立质量表 |

存储规则：

- usage：每个去重后的有效 Token 增量一行，不保存累计 JSON。
- session：每个 Session 元数据只保留一行，更新标题/关系时覆盖同一 `source_key` 对应记录。
- turn：每个 Turn 一行，`task_started` 建立，`task_complete` 补结束时间。
- quota：只有百分比、Reset、limit 或 plan 发生变化时新增；相同状态只更新 `last_seen_at_ms`。
- `thread_settings_applied` 只更新解析上下文；模型/tier 写到后续 usage 或 turn，不单独落行。

文件游标写入 `.runtime/jsonl-cursors.json`：每个文件只保存路径、inode、offset、mtime、末尾摘要，以及继续增量解析必需的当前 session/root/turn、model/tier、provider/plan 和上一份 cumulative Token 六字段。该 sidecar 只有每个 JSONL 文件一条小记录，不是事实库；丢失时允许全量重扫，`source_key` 保证数据库不重复。

### 4.2 `source_app_server`

一张表保存 `account`、`quota`、`usage` 三类精简快照。

| 字段 | 用途 |
| --- | --- |
| `id` / `source_key` | 主键与唯一状态摘要 |
| `kind` | `account/quota/usage` |
| `first_seen_at_ms` / `last_seen_at_ms` | 状态首次/最后确认时间；状态未变时只更新 last seen |
| `account_key` | 脱敏/HMAC 身份键 |
| `account_label` | 页面可显示的脱敏账号文本 |
| `auth_kind` | ChatGPT/API Key/Bedrock/unknown |
| `provider` | 当前 provider，可为空 |
| `plan_type` | Plus/Pro/其他，可为空 |
| `limit_id` / `window_kind` | quota 行的配额桶 |
| `used_percent` / `window_minutes` / `resets_at_ms` | 官方窗口状态 |
| `lifetime_tokens` | usage 行账号累计 Token 参考 |
| `daily_tokens_json` | 仅保存 `[{start_date,tokens}]`，不保存完整响应 |
| `freshness` | `pending/stale/settled/unavailable` |
| `status` | `ok/unavailable`，不保存冗长错误栈 |

不保存 `peakDailyTokens`、最长任务、连续天数、完整账号响应、认证 token、完整邮箱或购买 Credit/Reset 券字段，因为第一版页面不消费它们。

### 4.3 `source_ccusage`

一行对应一个“范围键 + 计价方案 + speed”的校验结果。

| 字段 | 用途 |
| --- | --- |
| `id` / `source_key` | 主键与唯一结果摘要 |
| `run_at_ms` | 执行时间 |
| `range_start_ms` / `range_end_ms` | 校验范围 |
| `scope` | `daily/session` |
| `scope_key` | 日期或 ccusage 原始 Session ID；root 合并在查询时完成 |
| `pricing_scheme` | `subscription/api` |
| `speed` | `auto/standard` |
| Token 六字段 | 与 JSONL 同口径的分类和总量 |
| `amount` | 对应方案计算值 |
| `model_breakdown_json` | 仅保存 `{model,tokens,amount}` 列表 |
| `ccusage_version` / `pricing_version` | 解释结果所需版本 |
| `status` | `ok/failed/incomparable` |

不保存 stdout 原文、扫描文件列表、完整命令环境或独立差异行。JSONL/ccusage 差值在 API 查询时计算。

### 4.4 `usage_daily`

一行对应一个本地日期；混合账号/套餐时保留总量，但占比为 `NULL`。

| 字段 | 用途 |
| --- | --- |
| `local_date` | 主键，`YYYY-MM-DD` |
| `account_key` / `auth_kind` / `plan_type` / `capacity_profile` | 当日上下文；混合时按质量标记 |
| Token 六字段 | 本机当日增量 |
| `credit` / `api_usd` | 本机当日派生值 |
| `local_percent` | 当日 Credit / 当日有效容量 |
| `account_tokens` | App Server 账号日 Token 参考 |
| `unobserved_tokens` / `coverage_ratio` | 仅可比较日期计算 |
| `account_token_freshness` | `pending/stale/settled/unavailable` |
| `official_percent_start` / `official_percent_end` | 当日首末官方已用百分比 |
| `official_percent_delta` | Reset 分段后计算的当日账号变化 |
| `reset_count` | 当日发生 Reset 的次数 |
| `quality` | 质量字符串 |

ccusage 值不复制到该表；按 `local_date` 从 `source_ccusage` 关联。

### 4.5 `usage_minute`

一行对应“分钟 + 账号 + window”。没有本机用量但有官方采样的分钟也保留。

| 字段 | 用途 |
| --- | --- |
| `id` / `bucket_key` | SQLite 主键和唯一分钟分桶摘要 |
| `minute_start_ms` | 分钟起点 |
| `local_date` | 日期索引 |
| `account_key` / `auth_kind` / `plan_type` / `provider` / `capacity_profile` | 归一化后的当时账号上下文 |
| `window_id` | `account_key + limit_id + window/reset` 的稳定摘要 |
| `window_start_ms` / `resets_at_ms` | Reset 区间 |
| `reset_marker` | 本分钟是否发生 Reset |
| Token 六字段 | 本机本分钟增量 |
| `credit` / `api_usd` | 本机本分钟增量 |
| `official_used_percent` | 本分钟采用的官方已用值 |
| `official_source` | `app_server/jsonl/none` |
| `quality` | 缺样、边界、混合账号等状态 |

`bucket_key` 由分钟、账号和 window 的真实值生成；业务字段允许 `NULL`，不使用伪造账号或窗口 ID。页面累计值均由分钟增量计算，不重复存累计列。

### 4.6 `usage_window`

该表物化每个账号、limit 和窗口类型对应的一次 Reset 区间。它是页面按周窗口和当前窗口的直接读模型；详细分钟曲线仍从 `usage_minute` 读取。

| 字段 | 用途 |
| --- | --- |
| `window_id` | 稳定窗口键 |
| `account_key` / `limit_id` / `window_kind` | 账号、官方 limit 和 primary/secondary |
| `window_start_ms` / `resets_at_ms` / `window_minutes` | Reset 区间边界 |
| `auth_kind` / `plan_type` / `provider` / `capacity_profile` | 窗口上下文 |
| Token 六字段 | 窗口内本机增量 |
| `credit` / `api_usd` / `local_percent` | 窗口派生金额和本机占比 |
| `account_tokens` / `unobserved_tokens` / `coverage_ratio` | 账号日 Token 参考和覆盖率 |
| `official_percent_start` / `official_percent_end` / `official_percent_delta` | 窗口官方变化 |
| `quality` | mixed_plan、边界或缺失状态 |

第二批 Pipeline 每次从三张来源表完整重建该表，避免前端或 API 对每个请求重新扫描分钟记录。

### 4.7 `usage_session`

该表实际以 Turn 为最小行；页面用 `local_date + root_session_id` 分组为一个 Session。这样既不增加 Turn 表，也不把所有轮次塞成巨大 JSON。

| 字段 | 用途 |
| --- | --- |
| `id` / `row_key` | SQLite 主键和唯一 Turn 分段摘要 |
| `local_date` | Turn 归属日期 |
| `root_session_id` | 页面合并后的对话 ID |
| `session_id` | 实际来源 Session/Thread ID |
| `turn_id` | Turn ID |
| `title` | 根对话标题 |
| `relation` | `main/child/fork/unknown` |
| `started_at_ms` / `ended_at_ms` | Turn 起止 |
| `window_id` | 本段所属 Reset 窗口 |
| `account_key` / `auth_kind` / `plan_type` / `provider` / `capacity_profile` | 本段归一化账号上下文 |
| `primary_model` | 主要模型，可为空 |
| `fast_state` | `fast/standard/mixed/unknown` |
| `model_breakdown_json` | 仅保存 `{model,tier,reasoning_effort,tokens,credit,api_usd}` |
| Token 六字段 | Turn 本机用量 |
| `credit` / `api_usd` | Turn 派生值 |
| `official_percent_start` / `official_percent_end` | 仅在真实采样覆盖 Turn 时填写 |
| `quality` | incomplete/mixed_model/mixed_account 等 |

一个 Turn 跨本地日期或 Reset 时，按日期/window 拆成多段写入，Token 和金额按实际增量分配；页面仍按同一 `turn_id` 合并展示。无法确定 window 时 `window_id=NULL` 并标记质量。没有 Turn ID 时使用稳定 `row_key` 去重，但对外仍显示 `turn_id=NULL`，不伪造业务 ID。

Session 汇总规则：

- `root_session_id` 相同的 main/child/fork Turn 合并成页面一条对话；第二批不再沿父链猜测根 ID。
- replay/fork 的重复 Token 先在 `source_jsonl` 去重，再进入 Turn 汇总。
- 标题优先根 Session；没有可靠标题时显示“未命名对话”。
- Session 开始/结束取所有成员 Turn 的最小/最大时间。
- Session 模型和 Fast 状态从 Turn 汇总，不给整个 Session 强行指定单一值。
- 标题优先读取 JSONL 自带元数据；缺失时只从本机 `session_index.jsonl` 或 `state_5.sqlite.threads.title` 补标题。它们属于本地 Session 元数据补充，不成为第四个用量来源，也不读取 `first_user_message`、preview、cwd 或 Git 信息。

### 4.8 `capacities`

只保存用户确认值，不保存自动候选。

| 字段 | 用途 |
| --- | --- |
| `id` | SQLite 主键 |
| `profile_code` | `usd20/usd100/usd200` |
| `account_key` | 可选；用于账号到容量档的人工映射 |
| `plan_type` | 仅作显示证据，不自动决定 profile |
| `weekly_credit` | 人工确认周容量 |
| `effective_from_ms` / `effective_to_ms` | 生效区间 |
| `confirmed_at_ms` | 确认时间 |

唯一约束使用 `(profile_code, account_key, effective_from_ms)`。自动估算建议、滑动区间和中间候选不落库。

## 5. Reset 与来源优先级

### 5.1 Reset 识别

优先使用真实配额字段：

1. 同一账号和 limit 的 `resets_at_ms` 变化；
2. `used_percent` 从较高值明显回落到接近 0；
3. App Server/JSONL 任一来源观察到新窗口；
4. 两边都有时保留两边源记录，报表当前值优先 App Server。

`window_id` 由账号、limit、窗口类型和窗口起点/Reset 组成；完整窗口结果物化到 `usage_window`。Reset 前后的分钟不得连接累计曲线。

窗口起点优先取实际观察到的百分比回落/Reset 时刻；历史只看到 `resets_at_ms + window_minutes` 时，才使用 `resets_at_ms - window_minutes` 作为近似起点并标记 `boundary_approximate`。人工提前 Reset 不能被普通周期倒推覆盖。

### 5.2 官方来源优先级

- 当前状态：App Server 优先，JSONL 作为交叉验证和断线回退。
- 历史状态：JSONL 和已保存的 App Server 快照都保留；同分钟两者都存在时报告差异。
- 账号日 Token：只来自 App Server；JSONL 只提供本机日 Token。
- ccusage：只校验 JSONL 日级/Session 级结果，不提供官方百分比。

## 6. 页面到数据的闭环检查

| 页面能力 | 必要数据 | 八表落点 | 结论 |
| --- | --- | --- | --- |
| 当前账号/套餐 | account/auth/plan/provider | `source_app_server` | 覆盖 |
| 当前官方窗口 | used/reset/limit | `source_app_server`，JSONL 回退 | 覆盖 |
| 本机当前窗口累计 | 分钟 Token/Credit/USD/window | `usage_minute` | 覆盖 |
| 日历与日趋势 | 日 Token/Credit/USD/百分比 | `usage_daily` | 覆盖 |
| 账号 Token 参考 | daily bucket/freshness | `source_app_server` → `usage_daily` | 覆盖 |
| JSONL vs ccusage | 日/Session 同口径字段 | `usage_daily`/`usage_session` + `source_ccusage` | 覆盖 |
| JSONL quota vs App Server | 两边配额快照 | `source_jsonl` + `source_app_server` | 覆盖 |
| 模型汇总 | Turn 模型拆分 | `usage_session.model_breakdown_json` | 覆盖 |
| Session 合并 | root/child/fork/turn | `source_jsonl` → `usage_session` | 覆盖 |
| 分钟曲线 | 分钟增量和官方采样 | `usage_minute` | 覆盖 |
| Reset 窗口页 | window_id 结果 | `usage_window` | 覆盖 |
| 容量估算 | 同窗口 Credit 与官方变化 | `usage_window` + `usage_minute` + `capacities` | 覆盖 |
| 价格说明 | 版本化代码价格卡 | 静态配置/API methodology | 覆盖，无价格表 |

## 7. 精审修正

上一版七表草案方向正确，但以下内容不足，现已修正为八表：

1. 上一草案曾把 `auth_kind/account_key/capacity_profile` 放入 JSONL 来源表，但 JSONL 并不稳定提供这些字段；现改为来源表只保存真实观察，跨源账号/容量归属只写入三张结果表，未知时为 `NULL`。
2. 原先 `usage_session` 按原始 Session 一行会让 child/fork 合并和 Turn 明细冲突；现改为每 Turn 一行，页面按 root Session 聚合。
3. 原先 Turn JSON 没有可靠表达 Turn 内模型/Fast 切换；现用紧凑 `model_breakdown_json`，不新增表。
4. 原先分钟表只考虑本机用量，无法在空闲分钟保留官方曲线；现规定官方采样分钟也产生一行。
5. 原先没有窗口表后的恢复规则；现固定由第二批 Pipeline 将窗口结果物化到 `usage_window`，分钟表保留详细时间轴。
6. 原先 App Server 每次轮询都可能写行；现改为状态变化新增、状态不变只更新 `last_seen_at_ms`。
7. 原先日表缺账号 Token 的 freshness、未观测量和可比较状态；现已补齐。
8. 原先容量表不能表达账号在不同日期使用哪个容量档；现允许 `account_key + effective range` 做人工映射。

本机结构核对也确认 Turn 模型可实现：当前样本中约有 11,499 个 `token_count`、1,446 个 `task_started`、1,409 个 `task_complete`、1,517 个 `turn_context` 和 1,468 个 `thread_settings_applied`。活动 Turn 允许没有 complete；`turn_context.turn_id + model` 和 thread settings 能把 Token 增量归到 Turn、模型与 tier。标题补充源当前可见 24 条 `session_index` 记录和 50 条带标题的 `state_5` Thread，仅保存最终标题映射。

对照 ccusage 当前 Codex 适配器也确认：长上下文档位按单个已归一化 usage 事件的 `input_tokens` 和模型阈值判断，不需要把完整 cumulative JSON 留在数据库。`source_jsonl` 的增量 Token 六字段、模型和 tier 足以支持重算；原始 cumulative 只在扫描当下用于去重和差分。

结论：修正后的八表能够覆盖当前三个页面。仍无法从现有来源稳定获得的数据必须保持 `NULL`，包括历史稳定账号 ID、部分标题、未采样 Turn 边界、账号分钟 Token、官方累计 Credit 和另一台机器的精确用量。

## 8. 存储规模约束

- `source_jsonl` 只保存约万级有效 Token 增量、Session/Turn 元数据和变化后的 quota，不复制约 500 MiB 原始日志。
- `source_app_server` 相同状态合并，账号 usage 低频读取。
- `source_ccusage` 保存归一化汇总，不保存 stdout 原文。
- `usage_daily` 最多每天一行。
- `usage_minute` 最多约 1440 行/天/有效账号窗口，通常只写活动或配额采样分钟。
- `usage_window` 每个 Reset 区间一行；不保存原始配额响应。
- `usage_session` 每个 Turn 一行；不保存消息正文。
- JSON 字段只允许两个：`model_breakdown_json` 和 `daily_tokens_json`，且都使用无空白紧凑格式。

## 9. 当前实现状态

截至 2026-08-08，本契约描述的结构已全部实现并验证：

- 数据库由 `config/schema.sql` 直接建立八张目标表（无旧迁移残留）。
- JSONL 的 session/turn/usage/quota 已写入 `source_jsonl`；App Server 快照写入
  `source_app_server`；ccusage 校验结果写入 `source_ccusage`。
- 第二批 Pipeline（`src/pipelines/result/materialize.rs`）从三张来源表重建
  `usage_daily`、`usage_minute`、`usage_window`、`usage_session` 四张结果表，
  支持重复重建与事务性替换。
- `/api/report` 读取真实日/分钟/窗口/Session/校验/价格数据，三个页面已用真实
  报告逐页验收；容量未确认时保留“待确认”，不使用 0 或近似值填充。

## 10. 实施门禁（已完成）

- 备份与 schema 迁移：已确认 `.runtime/codex-meter.sqlite` 可由 JSONL 重建，
  新 baseline 只创建八张目标表。
- fixture 验证：Session→Turn→root 合并、去重、模型/Fast、Reset 均有回归测试。
- 双源对账：JSONL 与 ccusage 的 Token 分类和总量可逐列比较（20 日总量差 0.222%，
  手动刷新后逐日差异收敛为 0）。
- App Server 断线时 JSONL 历史仍可展示；当前官方状态标记不可用。
- 后续任何新增字段必须先指出页面消费者；任何新增表必须先修改本文件并获得用户确认。

## 11. 实现追踪

| 计划任务 | 实现文件 | 测试/真实验收 | 页面/API 验收 | 当前状态 |
| --- | --- | --- | --- | --- |
| 来源事实采集 | `config/schema.sql`, `src/db.rs`, `src/pipelines/source/*` | `cargo test`；首见/末见、reset、replay、标题/父关系、reasoning、total-only 和 unavailable 回归测试 | 三个 source adapter 只写三张 `source_*` 表 | 已验证 |
| 真实历史回填与 ccusage 交叉验收 | `.runtime/codex-meter.sqlite`, `.runtime/jsonl-cursors.json` | 50 文件/83,347 行；source `50/1552/12481/101`；ccusage 8/8 成功、280 行；`integrity_check=ok`；20 日可对账、总量差 0.222% | JSONL/ccusage 差异在报告层解释 | 已验证 |
| 结果表物化（Daily/Minute/Window/Turn-Session） | `src/pipelines/result/materialize.rs`, `src/db.rs` | `cargo test`；窗口、跨日 Turn、去重和 reasoning_effort 回归测试 | 四张结果表可重复重建；Reset/周窗口直接读取 `usage_window` | 已验证 |
| 页面与真实数据闭环 | `src/service/report.rs`, `web/index.html` | `cargo build`；`cargo test`；内嵌 Web JS 语法检查；`.runtime/codex-meter.sqlite` 完整性和八表计数检查 | `/api/report` 读取真实数据；三个页面逐页验收 | 已验证 |
| 首次范围回填与 Pipeline 性能 | `service.sh`, `src/main.rs`, `src/pipelines/source/jsonl*`, `src/db/source_jsonl.rs` | 默认 30 天、显式 `--from`、增量 cursor、fork/replay 回归；release 冷启动与 ccusage 对比基准 | 不改变八张表和 source 字段语义 | 进行中 |
