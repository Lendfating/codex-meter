# Codex Meter 第一批来源 Pipeline

状态：2026-08-07，M1 来源事实链与真实历史回填已验收

这条 Pipeline 只做一件事：把三个来源采集、脱敏、归一化后写入三张 `source_*` 表。它不计算日、分钟、Session 或 Reset 报表；那些属于第二批 Pipeline。

```text
JSONL 文件 ───────┐
Codex App Server ──┼─> 轻量 adapter / 去重 / NULL 质量标记 ─> source_* 三张表
ccusage CLI ──────┘
```

## 1. 三个来源的职责

| 来源 | 写入表 | 负责什么 | 不负责什么 |
| --- | --- | --- | --- |
| Codex JSONL | `source_jsonl` | 本机 Session、Turn、Token 增量和历史 quota | 账号总 Token、官方当前状态 |
| App Server | `source_app_server` | 当前账号、套餐、官方 quota、账号日 Token | 本机 Session 明细 |
| ccusage | `source_ccusage` | JSONL 的 daily/session 独立校验 | 生产账本、官方百分比、Reset 事实 |

ccusage 仍然是验证器。它可以重新读取 JSONL，但不能覆盖 `source_jsonl` 的本机事实，也不能被当作官方账号数据。

## 2. 采集频率

### 2.1 JSONL

- 启动时扫描 `sessions/` 和 `archived_sessions/`，完成历史回填。
- 新来源第一次运行从文件头回填 `source_jsonl`；之后使用与数据库同目录的
  `*.jsonl-cursors.json` sidecar 保存偏移，不把游标混入八张正式表。
- 默认每 10 秒扫描一次；只读取文件游标之后的新完整行，文件未变化时快速跳过。
- 最后一行没有换行符时暂不处理，下一轮继续。
- 文件被截断或游标超出长度时从文件头重扫；`source_key` 保证不会重复写入。
- sidecar 只保存路径、偏移和摘要等可重建游标；删除 sidecar 后会安全地从文件头重扫，
  由 `source_key` UPSERT 去重。

JSONL 行归一化规则：

| JSONL 事件 | `source_jsonl.kind` | 唯一键 |
| --- | --- | --- |
| `session_meta` | `session` | `session:<session_id>` |
| `task_started/task_complete` | `turn` | `turn:<session_id>:<turn_id>` |
| `token_count` | `usage` | `usage:<event_digest>` |
| rate-limit 状态 | `quota` | quota 字段状态摘要 |

只写 Token 六字段、模型/tier、Session/Turn 关系、quota 字段和紧凑质量标记；不写原始 payload、正文、提示词或工具输入输出。

`session_meta` 优先使用当前日志的 `payload.id`；`payload.session_id` 仅作为旧格式回退，
因为子任务日志中的 `session_id` 可能指向父线程。只有包含 input/cache/output/reasoning
至少一个可计价分项的 `token_count` 才写入 usage；只有 `total_tokens` 的不完整快照会被
跳过（quota 观测仍保留）。

### 2.2 App Server

App Server 轮询只保留三个方法：

- `account/read`：启动时和每 6 小时一次；
- `account/rateLimits/read`：启动时和每 60 秒一次；
- `account/usage/read`：启动时和每 6 小时一次。

当前服务只有设置 `CODEX_METER_APP_SERVER_ON_BOOT=1` 才启动这个轮询，避免本地没有 App Server 时反复拉起外部进程。失败时不写零值；没有成功快照时由查询层标记为 `unavailable`，并保留 JSONL 历史。

同一个脱敏状态摘要只保留一行，重复轮询只更新 `last_seen_at_ms`。账号身份只保存哈希键，不保存邮箱、Access Token 或完整响应。

### 2.3 ccusage

- 启动执行由 `CODEX_METER_CCUSAGE_ON_BOOT=1` 控制；手工刷新执行由 `CODEX_METER_CCUSAGE_ON_REFRESH=1` 控制。
- 一次运行固定执行 8 组：`daily/session × subscription/api × auto/standard`。
- 只有 JSONL 本轮有新行或用户手工刷新时才适合执行；不按秒轮询。
- 每个日期或原始 Session 一行，保存 Token 六字段、金额、模型紧凑汇总、参数和版本。
- 命令失败也记录 `failed` 状态和范围键，不把失败伪装成零。

## 3. 去重与失败口径

1. 来源记录都使用稳定 `source_key`；重复扫描/重复轮询使用 UPSERT。
2. JSONL 的 quota 相同状态更新 `last_seen_at_ms`，状态改变才产生新行；
   `observed_at_ms` 保留该状态第一次被看见的时间。
3. App Server 相同 account/quota/usage 状态合并，不按轮询次数无限增长。
4. ccusage 每次运行保留版本和参数；不同运行可比较，但不覆盖历史校验结果。
5. 缺字段使用 `NULL` 和 `quality/freshness/status`，不填 `0`。
6. 任一来源失败不阻断其他来源；三张来源表职责独立。

## 3.1 已确认的源字段语义（M1）

- `source_jsonl.kind=quota` 保存官方当前快照（`used_percent`、窗口和
  `resets_at_ms`），不是差值；同一状态只有一行，`observed_at_ms` 是首见时间，
  `last_seen_at_ms` 是最近确认时间。
- `source_app_server` 的三类状态同样使用稳定状态摘要；同状态轮询只更新
  `last_seen_at_ms`，状态变化产生新行，`first_seen_at_ms` 永不被覆盖。
- JSONL 的累计 Token 下降被视为计数器 Reset：该次累计值作为新的增量写入，且
  `quality` 标记 `counter_reset`，不得用饱和减法丢掉 Reset 后的 Token。
- `source_jsonl.parent_session_id` 保存直接父 Session；第一批扫描时沿完整父链写入最终
  `root_session_id`，无法解析时保留 `NULL` 和质量标记。`session_index.jsonl` 与可读的
  `state_5.sqlite` 只补标题、父边和 `reasoning_effort`，不复制正文或认证信息。
- `reasoning_effort` 从 `turn_context`、`thread_settings_applied` 和本地线程元数据按
  观察时序写入 Turn/usage；未知时为 `NULL`，不猜测默认值。
- `ccusage.inputTokens` 是扣除 Cache Read 后的非缓存输入；对账时 JSONL 使用
  `input_tokens - cache_read_tokens` 与它比较，不能直接比较原始输入字段。

## 4. 第一批和第二批的边界

第一批只产生：

- `source_jsonl`
- `source_app_server`
- `source_ccusage`

第二批再从这三张表生成：

- `usage_daily`
- `usage_minute`
- `usage_window`
- `usage_session`

第二批负责日期切分、Reset 分段、模型/Session 聚合、Credit/API 计价和页面 report；第一批不在采集时提前写这些结果，避免来源变化后无法重算。

## 5. 当前代码状态

- JSONL adapter 已把 `session_meta`、Turn 起止、`token_count` 和 quota 写入 `source_jsonl`，不再写旧采集表。
- App Server adapter 已把 account/quota/usage 精简快照写入 `source_app_server`，不再镜像旧账号/配额表。
- ccusage adapter 已把每次 daily/session 结果归一化写入 `source_ccusage`，不再写旧校验表；报告层在内存中按来源快照生成对账。
- 调度入口仍在现有单服务进程中：JSONL 10 秒循环、App Server 按上述轮询、ccusage 启动/手工刷新。没有新增后台服务。

第二批已接入启动、JSONL 刷新、App Server 刷新和手工刷新路径：每次运行从三张
`source_*` 表可重建 `usage_daily`、`usage_minute`、`usage_window`、`usage_session`。
Reset/周窗口直接物化到 `usage_window`，分钟表保留详细时间轴；未知计价和缺失边界保留
`NULL` 与质量标记。

## 6. 2026-08-07 真实回填与 ccusage 交叉验收

验收库为 `.runtime/codex-meter.sqlite`，只包含真实 JSONL 和 ccusage 归一化结果；测试库及
中断验收库均移出项目目录。真实 JSONL 首次扫描 50 个文件、83,347 行，写入：

- `session=50`、`turn=1,552`、`usage=12,481`、`quota=101`；
- `parent_session_id` 非空 2,079 条，未发现自父关系；
- 不完整 total-only usage 为 0；SQLite `PRAGMA integrity_check` 为 `ok`；
- ccusage `daily/session × subscription/api × auto/standard` 共 8 组、280 行，8/8 成功。

20 个日期均有两边数据。按统一 Token 口径，JSONL 与 ccusage 的 `total_tokens` 合计分别为
1,859,787,547 与 1,855,676,191，差 4,111,356（0.222%）。这证明两边可以完整逐日、逐
Session 对账，但当前不能宣称逐字段完全相等：JSONL 是原始本机事实，保留 fork/重放和扫描
期间追加的记录；ccusage 会按父链去掉重放前缀。该差异必须由第二批 Session/Reset 聚合解释，
不能把 ccusage 覆盖成生产账本。
