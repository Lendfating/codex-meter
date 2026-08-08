# Token 使用参考维度

## 1. 定位

Token 维度是 Codex Meter 的**参考数据源**，不是订阅 Credit 或周窗口的权威账本。

它回答两个问题：

1. 这台机器从本地 JSONL 观察到每天产生了多少 Token。
2. 当前 ChatGPT 账号的账号级接口在每天维度报告了多少 Token。

两者可以用来核对历史数据、发现本机采集缺口，以及在同一个账号被多台机器使用时估算“本机之外的未观测 Token”。它不能直接回答本机消耗了多少 Credit，也不能替代 `account/rateLimits/read` 的窗口剩余百分比。

## 2. 两个来源

### 2.1 本机 JSONL：本机事实源

Codex 本地会话文件主要位于：

```text
~/.codex/sessions/**/*.jsonl
~/.codex/archived_sessions/**/*.jsonl
```

其中 `token_count` 事件可以提供：

- `last_token_usage`：最近一次增量 Token；
- `total_token_usage`：当前线程累计 Token；
- 输入、缓存输入、输出、推理输出和总 Token；
- 事件时间、session/turn、模型、provider、Fast/Standard 状态上下文。

本机每日 Token 必须从去重后的增量事件或 session 累计值差分得到，不能把每条 `total_token_usage` 直接相加，否则会重复计算。JSONL 仍然是本机明细的主事实源，`ccusage` 可作为已有的去重、模型解析和价格计算引擎。

### 2.2 App Server：账号级每日 Token 画像

App Server 的 `account/usage/read` 返回账号侧的每日 Token 活动摘要，典型字段为：

```json
{
  "summary": {
    "lifetimeTokens": 1234567,
    "peakDailyTokens": 45678,
    "longestRunningTurnSec": 540
  },
  "dailyUsageBuckets": [
    { "startDate": "2026-06-18", "tokens": 12345 }
  ]
}
```

有价值的字段是 `lifetimeTokens`、`peakDailyTokens` 和 `dailyUsageBuckets[].tokens`。这个接口没有模型、输入/输出拆分、Fast 状态、价格版本或 Credit 拆分，因此只能进入 Token 参考维度。

官方 App Server 文档把该能力描述为账号级每日 Token 活动及按日 bucket；接口还依赖 Codex 服务端认证，并不是 API-key-only 用量账本：[Codex App Server README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)。

## 3. 日期对齐规则

### 3.1 当前决定

暂时按**本机配置的本地时区**对齐日期；当前项目默认值为 `Asia/Shanghai`。因此：

- JSONL 事件先转换为本机 `local_date`；
- App Server 原始 `startDate` 原样保存，同时按同一 `local_date` 作为比较键；
- 不把 App Server 的日期强行解释成 UTC 或美国时区；
- 数据库同时保存 `alignment_timezone`，以后可以在诊断页重新验证或更换规则。

App Server schema 当前只给 `YYYY-MM-DD`，没有公开说明这个日期的时区语义。因此“按本地日期对齐”是本项目当前的可操作决定，不应描述成官方保证。已有本机样本中，账号 bucket 与 `Asia/Shanghai` 的本机日汇总更接近，但仍需继续用多个已完成日期验证。

本次环境验证（2026-08-04）观察到：账号级接口最新 bucket 为 2026-08-03，而当天 bucket 尚未出现；同一 2026-08-03 日期下，账号 bucket 与本机 `Asia/Shanghai` 汇总接近，明显不同于用 UTC 或美国太平洋时区切日的结果。这个样本支持当前的本地日期决定，但只属于当前账号/环境的经验性证据，不能推广成 App Server 的通用时区承诺。

### 3.2 当前日期不视为已结算

账号级每日统计是异步画像，不是实时计数器。实际观察中，今天的 bucket 可能缺失，昨天的数据也可能在一段时间后才补齐；官方 Codex issue 也记录过 Profile 统计滞后或不完整的情况：[Codex Desktop profile usage statistics issue](https://github.com/openai/codex/issues/25479)。

因此每个账号日期要有 freshness 状态：

- `pending`：今天或最近一天尚未出现，不能用于结论；
- `stale`：接口有返回，但仍可能在后台补写；
- `settled`：已超过观察窗口，并且两次间隔读取结果稳定；
- `unavailable`：接口失败或该认证类型不支持账号画像。

采集策略：启动/重连时读取一次；之后每 6 小时读取一次；本机日期跨天后额外读取一次。不要为了追踪当天 Token 而每分钟拉取这个接口。`account/rateLimits/read` 的窗口百分比仍由独立的实时配额采集策略负责。

## 4. 两台机器的参考计算

对同一账号、同一 `local_date`，且两个来源的统计口径可比较时：

```text
account_tokens(d) = App Server dailyUsageBuckets[d].tokens
local_tokens(d)   = 本机 JSONL 去重后的每日 Token
unobserved_tokens(d) = max(account_tokens(d) - local_tokens(d), 0)
coverage_ratio(d) = local_tokens(d) / account_tokens(d)
```

在 200 美元账号同时运行于两台机器的场景中，`unobserved_tokens` 可以作为“另一台机器 + 其他未观测来源 + 口径误差”的粗略估计。它不是另一台机器的精确 Token，也不能在没有第二台机器数据时拆出第二台机器的真实值。

当 `account_tokens(d)=0` 时 `coverage_ratio` 必须为 null；当本机 Token 大于账号 Token 时，差额只显示为 0 并降级为不可比较/采集异常，不能把负数解释成另一台机器“使用了负 Token”。

只有满足以下条件才允许展示差额：

- account bucket 已经是 `settled`；
- 本机日期、账号身份和 App Server 日期键已经对齐；
- 本机事件确实归属于该 ChatGPT 账号；
- 不混入无法确认的第三方 API 或另一个账号；
- 当天没有明显 JSONL 缺读、重写或归档去重异常。

否则显示“不可比较”，不要显示负数或伪造覆盖率。

## 5. 为什么不能用 Token 直接算 Credit

同样数量的 Token 可能因为模型、输入/缓存/输出构成、长上下文、价格版本和 Fast 状态而产生不同 Credit。账号级每日 Token 又没有这些拆分，因此：

- Token 参考值不进入首页正式的订阅窗口百分比；
- 不用 `account_tokens` 反推 20/100/200 周容量；
- 不用 `coverage_ratio` 代替本机 Credit 占比；
- 本机正式 Credit 仍来自 JSONL + `ccusage` + 版本化价格 + Fast 差分；
- App Server 的窗口百分比仍作为账号总量控制面和容量标定证据。

Token 维度可以帮助发现“本机 Credit 曲线和账号窗口曲线明显不一致”，但只能触发诊断或降低质量等级，不能自动改写人工确认的容量。

## 6. 存储设计

`account_usage_snapshots` 保存每次 App Server 原始读取：

- `observed_at`、`account_id`、`source`；
- `lifetime_tokens`、`peak_daily_tokens` 等 summary；
- 原始 `startDate`/`tokens` bucket JSON；
- `alignment_timezone`、接口版本、请求状态和错误摘要。

另建派生表 `token_reference_daily`，一行对应一个账号和本机日期：

| 字段 | 含义 |
| --- | --- |
| `local_date` | 本机时区日期，当前为 Asia/Shanghai |
| `account_tokens` | 账号级 daily bucket Token |
| `local_tokens` | 本机 JSONL 去重 Token |
| `unobserved_tokens` | 非负差额，仅在可比较时计算 |
| `coverage_ratio` | 本机 Token / 账号 Token |
| `freshness` | pending/stale/settled/unavailable |
| `quality` | exact/reference/incomparable/missing_samples |
| `source_snapshot_ids` | 参与派生的原始快照 |

保留原始 bucket，不把远程日期改写掉；派生结果可以在时区规则变化后重算。账号级 Token 与本机 Credit 日汇总分开存，避免 API 返回层把两个单位混在一起。

## 7. 页面展示

### 用量主页

在每天详情或趋势图的参考区域增加：

- 本机 Token；
- 账号 Token；
- 未观测 Token/覆盖率；
- `pending`/`stale`/`settled` 标签；
- “参考维度，不参与 Credit 百分比”的固定说明。

当前日期只显示本机实时值，账号级 Token 显示“等待账号统计”或最近一次已结算值。历史已结算日期才绘制本机/账号 Token 对比线。

### 容量标定页

在候选容量证据面板中可显示 Token 对比作为辅助证据：

- 账号 Token 与本机 Token 是否同量级；
- 是否存在明显未观测差额；
- 当前日期或远程数据是否延迟。

它不能直接改变 `confirmed` 容量；容量仍由人工确认后保存。

## 8. 验收与后续验证

至少覆盖以下测试：

1. 同一日期按本机时区转换，跨午夜不落到 UTC 默认日期。
2. 当前日期缺少远程 bucket 时标记 `pending`，不把缺失当作 0。
3. 远程 bucket 延迟补齐后，历史日期从 `pending/stale` 变为 `settled`，不重复累加。
4. 两次 App Server 读取内容相同才可提升稳定性；读取失败不覆盖上一份有效快照。
5. 本机 Token 大于账号 Token 时不产生负的“另一台机器用量”，而是记录 `incomparable` 或 0 差额并解释原因。
6. 混入 API/第三方 provider 或账号切换时禁止展示覆盖率。
7. 本机 Token 与账号 Token 只作为参考，不改变 Credit、价格版本和人工容量确认结果。
