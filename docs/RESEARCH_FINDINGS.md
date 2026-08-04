# 调研结论

## 1. 结论摘要

Codex 当前没有公开一个“本周已经消费多少订阅 Credit”的累计字段。可获得的数据分成三类：

| 数据 | JSONL | App Server | 含义 |
| --- | --- | --- | --- |
| 本机 token 明细 | 有 | 当前线程通知有 | 本机产生的输入、缓存输入、输出、推理 token |
| 账号窗口百分比 | 有，随 token 事件落盘 | 有，读取与推送 | 当前登录 ChatGPT 账号的总窗口状态，不是本机独占状态 |
| 登录身份 | 历史日志不完整 | 有 | ChatGPT/API Key/Bedrock，以及 ChatGPT 邮箱和套餐 |
| Credit 余额 | 可随配额快照出现 | 有 | 额外/可购买 Credit 余额，不是订阅周窗口容量 |
| 累计 Credit 消耗 | 没有 | 没有 | 必须由本机 token、模型、价格和 Fast 倍率推算 |
| 账号累计 token | 没有独立汇总接口 | 有 | lifetimeTokens 与按日 tokens；不是 Credit，也不等于周窗口消耗 |

因此系统采用三源互补：JSONL 负责精细本机事件，App Server 负责账号与官方总配额，`ccusage` 负责成熟的解析、去重和计价。

## 2. JSONL 能拿到什么

数据位于 `~/.codex/sessions/**/*.jsonl` 和 `~/.codex/archived_sessions/**/*.jsonl`。

### 2.1 token_count 事件

已在本机 41 份历史会话中核对到：

- `info.total_token_usage`：当前线程累计 token。
- `info.last_token_usage`：最近一次增量 token。
- 两者均可含 `input_tokens`、`cached_input_tokens`、`output_tokens`、`reasoning_output_tokens`、`total_tokens` 和 `cache_write_input_tokens`。
- `model_context_window`：模型上下文窗口。
- `rate_limits`：服务端随响应返回的账号配额快照。

`rate_limits` 可含：

- `limit_id`、`limit_name`。
- `primary` 和 `secondary` 的 `used_percent`、`window_minutes`、`resets_at`。
- `plan_type`。
- `credits.has_credits`、`credits.unlimited`、`credits.balance`。
- `individual_limit`、`spend_control_reached`、`rate_limit_reached_type`。

这些百分比来自服务端账号配额，因此是同一 ChatGPT 账号跨机器的总状态。JSONL 只是把当时看到的账号状态记录到本机，不代表这个百分比全部由本机造成。

### 2.2 模型与 Fast

- 会话元数据含 `model_provider`，token 事件含模型信息。
- `thread_settings_applied.thread_settings.service_tier` 可记录 `priority`/`fast` 与 `default`/`standard` 的切换。
- Fast 状态按事件继承，能覆盖同一个会话内“开、关、再开”的情况。
- 当前 `ccusage` Codex 适配器已经按 token 事件继承 Fast 状态，并按模型的 `fastMultiplier` 计价。

`ccusage --json` 的最终模型汇总不会单独列出 Fast token，但可以分别执行自动模式和强制 Standard 模式：

```text
Fast 附加 Credit = 自动模式 Credit - 强制 Standard Credit
```

这样不需要修改 `ccusage`，也不会因汇总结果隐藏 Fast 明细而丢失最终计价差额。

### 2.3 JSONL 不提供的身份信息

对本机全部历史 JSONL 的结构化字段检查没有发现：

- ChatGPT 邮箱或稳定账号 ID。
- `authMode`。
- API Key 标识。
- 历史 `base_url` 或完整 provider endpoint。
- 订阅周窗口总 Credit 或累计已用 Credit。

`session_meta.payload.model_provider` 只是 provider 配置名，不是套餐名。本机样本中：

- 20 个 `model_provider=openai` 会话同时出现 `plan_type=plus`。
- 21 个 `model_provider=pro` 会话没有 `plan_type`。

因此这里的 `pro` 不能解释为 Pro 套餐；它是用户定义的 API provider/profile 名称。用户已经确认这 21 份记录属于 API 类，因此本项目将它们通过一条本地人工映射归入 UI 的 `Other/API`，同时保留原始 `model_provider=pro` 和 `plan_type=null`。这个映射只适用于当前用户的数据，不能写成通用 Codex 识别规则。

## 3. App Server 能拿到什么

以下结论已用本机 Codex `0.146.0-alpha.3.1` 生成的 JSON Schema 核对。

### 3.1 account/read

- ChatGPT：`type=chatgpt`、`email`、`planType`。
- API Key：`type=apiKey`。
- Amazon Bedrock：`type=amazonBedrock`。
- `requiresOpenaiAuth`。

`account/updated` 还会推送：

- `authMode`：`apikey`、`chatgpt`、`chatgptAuthTokens`、`headers`、`agentIdentity`、`personalAccessToken`、`bedrockApiKey`。
- `planType`。

这使未来的 ChatGPT/API 切换可以在发生时建立明确的账号上下文区间。

### 3.2 account/rateLimits/read

- 一个或多个 `limitId` 配额桶。
- 主/次窗口的已用百分比、时长和重置时间。
- 套餐类型。
- 可选 Credit 余额、个人消费控制和触顶原因。
- `rateLimitResetCredits`：可用的“重置券”数量与状态。

`rateLimitResetCredits` 不是消费 Credit；`credits.balance` 也不是订阅周窗口容量。当前本机账号返回 `hasCredits=false`、`balance=0`，但账号仍有正常周窗口。

### 3.3 account/usage/read

可返回：

- `lifetimeTokens`。
- `peakDailyTokens`。
- `longestRunningTurnSec`。
- 连续使用天数。
- 可选 `dailyUsageBuckets[{startDate,tokens}]`。

这是账号侧 token 活动画像，可能跨机器，但没有模型、缓存、输出、Fast 或 Credit 拆分，不能直接用于周窗口 Credit 计算。该接口也可能暂时拉取失败，因此只能作为辅助校验源。

### 3.4 thread/tokenUsage/updated

当前线程实时通知包含：

- `threadId`、`turnId`。
- `last` 和 `total` 的输入、缓存输入、输出、推理输出、总 token。
- 可选模型上下文窗口。

它适合改善实时体验，但不替代 JSONL：通知不是完整历史账本，也没有 Fast、价格版本和账号容量。

## 4. 历史账号/API 能识别到什么程度

历史记录采用“证据等级”，不伪造确定性：

| 历史证据 | 分类 | 置信度 |
| --- | --- | --- |
| `rate_limits.plan_type=plus` | ChatGPT Plus | 高 |
| `rate_limits.plan_type=pro` | ChatGPT Pro | 高 |
| 其他非空 `plan_type` | ChatGPT 对应的其他套餐；UI 可汇总为 Other | 高 |
| `model_provider=pro`、`plan_type` 为空，且本用户已确认 | API；UI 汇总为 Other/API | 人工确认，高 |
| 其他自定义 `model_provider`，无套餐配额，且人工映射过该 provider | 第三方或自定义 API | 高/中 |
| `model_provider=openai`，无套餐配额 | 官方 API 或缺失配额的 ChatGPT | 不确定 |
| 未经人工确认、仅 provider 名为 `pro` | 仅表示 provider 名 | 不能据此判定 Pro 套餐 |

历史 JSONL 无 `base_url`，所以无法在所有旧会话中严格区分“官方 OpenAI API”和“把 base URL 改成第三方代理的兼容 API”。解决方式是：

1. 对已知 provider 名建立一次人工映射和时间范围。
2. 无证据的历史记录保留为 `unknown`，不自动归入 Plus/Pro。
3. 未来同时记录 App Server auth mode、provider 名和脱敏 endpoint 指纹。
4. 永不保存 API Key；endpoint 只保存主机名或本地 HMAC 指纹，不保存 query/header。

### 4.1 三个维度不能合并

实现必须分别保存：

- `auth_kind`：ChatGPT、API Key、自定义 API、Bedrock、未知。
- `plan_type_raw`：App Server/JSONL 返回的 `plus`、`pro`、其他值或空。
- `capacity_profile`：用户手工选择的 20/100/200 美元容量档位。

因此 `plan_type=pro` 可以确定为 ChatGPT Pro，但不应单靠它自动选择“100 美元”还是“200 美元”的本地容量档；容量档仍以人工配置为准。

## 5. 为什么仍需 App Server

JSONL 更适合历史 token 和 Fast 事件，但 App Server 仍不可删除：

- Codex 空闲时 JSONL 不会产生新 token 事件，无法持续观察重置和账号切换。
- JSONL 没有邮箱、稳定的登录方式或可靠的 API/ChatGPT 身份。
- App Server 能在下一次 token 产生之前推送账号变化。
- App Server 能返回全部配额桶，而 JSONL 只是在某次请求后附带当时快照。

最终职责划分：JSONL 是本机事实账本，App Server 是账号身份与总配额控制面，`ccusage` 是本机用量计算器。

## 6. 官方文档入口

- [Codex App Server](https://developers.openai.com/codex/app-server)
- [Codex Pricing](https://developers.openai.com/codex/pricing)
- [Codex Speed](https://developers.openai.com/codex/speed)
- [Codex rate card](https://help.openai.com/en/articles/20001106-codex-rate-card)
