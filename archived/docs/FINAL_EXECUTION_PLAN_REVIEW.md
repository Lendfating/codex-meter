# Codex Meter 最终执行计划精细化审查

## 1. 审查结论

《Codex Meter 最终执行计划》与最终产品目标一致，可以作为后续实现主线，但只能在本审查列出的 P0 门禁全部落实后进入新功能编码。

总体结论：

- 不推倒现有项目。
- 保留采集游标、SQLite 基础、隐私策略、fixture 和阶段 2 的可靠测试。
- 在 App Server 阶段之前重建业务语义层。
- JSONL 与内部算法是本机生产账本；JSONL 同时承担历史配额主证据，App Server 承担实时账号控制面，两者交叉认证；ccusage 是结果持久化且页面可见的独立验证器。
- 页面一是第一条端到端验收主线；容量页面不能早于事实、归因、计价和窗口全部稳定。

本次审查查阅了 [数据来源与职责边界参考](DATA_SOURCE_REFERENCE.md)。它是字段来源的辅助索引，不替代本计划或本审查的规范；执行时若实际接口与文档冲突，先保留原始数据并暂停该字段的正式派生，不能用默认值自行裁决。

## 2. 目标覆盖审查

| 最终效果 | 计划覆盖 | 结论 |
| --- | --- | --- |
| 每日 Token/Credit/API 美元 | daily rollup + 页面一 | 已覆盖 |
| 每日账号和百分比变化 | context + quota window/day segments | 已覆盖 |
| App Server 之前的历史百分比和 Reset | JSONL rate-limit backfill | 已覆盖 |
| 分钟 Token、Credit、百分比轨迹 | minute rollup + quota observations | 已覆盖 |
| Session 标题、模型、Fast、成本 | sessions + App Server metadata + session rollup | 已覆盖 |
| 主 Session/子代理/fork 合并 | session graph + conversation rollup | 已覆盖 |
| 账号每日 Token 与本机对照 | normalized buckets + token reference | 已覆盖 |
| 推算 20/100/200 周容量 | calibration engine + manual confirmation | 已覆盖 |
| 公式和价格历史说明 | methodology API/page | 已覆盖 |
| 内部结果与 ccusage 人工对比 | persisted reconciliation + day/session panel | 已覆盖 |
| 两个人共享账号时区分本机与总账号 | local facts + account percent + unobserved | 已覆盖但必须保留误差措辞 |

没有发现页面目标缺少底层数据路径。关键风险集中在重复计数、Session 关系、边界切分和容量污染，而不是页面本身。

## 3. P0 阻断项

### P0-1 JSONL 不能继续丢弃累计 Token 证据

现状：当前采集器优先写 `last_token_usage`，缺失时直接回退累计值。连续累计值可能被当作多个增量相加。

要求：

- 同时保存 last 和 cumulative。
- 独立 normalization 阶段产生 delta。
- 累计未变化时抑制重复 last。
- 累计回退时开启 counter epoch，而不是静默饱和减法。

门禁测试：重复累计、缺 last、首次中途接入、counter reset 四类 fixture。

### P0-2 fork/subagent replay 必须先去重再聚合

风险：fork 子日志会复制父历史。直接按 Session 求和会让父历史重复计入；直接把父子 Session totals 合并同样会重复。

要求：

- 从 `forked_from_id` 和 `source.subagent.thread_spawn.parent_thread_id` 建图。
- 父文件存在时精确匹配 replay prefix。
- 父缺失启发式必须降级质量。
- conversation rollup 只汇总已经 replay-safe 的 delta。

门禁测试：父完整、父缺失、父在 fork 后继续使用、三层子代理、并发相同 Token。

### P0-3 原始 Session 和展示对话组必须分离

风险：如果为了页面方便直接改写 Session ID 或把子 Session 并入父 Session，ccusage 对账、跨日拆分和人工修订都会失去可追溯性。

要求：

- 原始 `session_id` 永不改变。
- 关系和 root 是独立版本化投影。
- 页面明确支持 raw/conversation 两种口径。
- 人工合并不能修改底层事实。

### P0-4 ccusage 必须是验证器而不是生产账本

风险：若继续通过周期性 ccusage 累计快照差分生成生产增量，将产生扫描成本、切换边界和版本耦合，并与“JSONL 主事实源”的目标冲突。

要求：

- ccusage 结果只写 reconciliation 表。
- 日级、原始 Session 级结果、白名单原始 JSON 和逐项差异必须持久化并可由页面查询。
- `usage_deltas` 只能由 JSONL、账号上下文和内部计价生成。
- ccusage 失败不能阻塞主账本。
- 固定版本、offline、同 timezone/pricing/speed 对账。
- 发布前跑完整历史；运行期按受影响范围自动重跑，并支持用户手工重跑。

### P0-5 日期、账号和价格必须统一使用半开区间

风险：闭区间或按 Session 开始日归属会在午夜、账号切换和价格切换处重复/漏计。

要求：

```text
[start, end)
```

统一用于本地日期、账号上下文、价格版本和 Reset 窗口。跨午夜 Session 按事件拆分。

门禁测试：边界前后 1ms、跨日 Session、同一时刻账号/价格切换、DST。

### P0-6 计价必须保留请求级长上下文信息

风险：长上下文阈值按单次请求判断。若只保存日/Session 汇总，无法事后恢复哪些输出和缓存属于长上下文请求。

要求：

- 在 usage delta 生成时记录请求是否进入长上下文 tier。
- 阈值属于价格版本和模型。
- 整次请求切换价格的模型不能使用边际分段公式。

### P0-7 未知金额不得用 0 代替

风险：未知模型、Unknown Fast 或缺价格时返回 0，会让用户误以为没有消耗，并污染容量估计。

要求：

- 正式金额为 null。
- Unknown Fast 返回 Standard–Fast 范围。
- `missing_pricing/fast_unknown` 阻止容量样本进入拟合。

### P0-8 schema 重整前必须确认真实数据库迁移策略

仓库中未发现正式数据库，当前适合整理 baseline。但不能假设工作区外不存在用户数据库。

要求：实施 R0 时明确二选一：

1. 没有需保留数据库：重整 baseline，从空库重建。
2. 有需保留数据库：追加迁移、先备份、写转换校验。

不得先重写 migration，之后才发现真实数据需要保留。

### P0-9 Rust 测试工具链必须先恢复

本次审计中 `cargo test` 未执行，原因是 Homebrew Rust 缺少 `/opt/homebrew/opt/llvm/lib/libLLVM.dylib`。在无法运行测试时重构事实模型不可接受。

要求：R0 首先恢复可重复工具链，并让 `just check` 覆盖 Rust、Web、fixture 和 schema。

### P0-10 JSONL 历史配额不能降级为附带字段

风险：App Server 只能提供运行后的实时采样，无法恢复服务启动前的历史百分比和 Reset。如果 JSONL 只抽 Token，页面将永久缺失最重要的历史窗口趋势。

本机只读结构审计提供了直接证据：45 个 JSONL 文件中有 8,438 条 `token_count/rate_limits`，4,357 条包含 `used_percent/window_minutes/resets_at`。这既证明历史数据可回填，也证明它是稀疏采样而不是连续时间序列。

要求：

- 全历史 backfill 独立抽取 JSONL primary/secondary rate limits。
- 保存 `used_percent/window_minutes/resets_at/limit/plan/credits` 和来源事件。
- 在没有 App Server 的历史区间由 JSONL 构建稀疏 quota timeline。
- 与 App Server 重叠时逐字段认证，保留一致、取整差和冲突结果。
- 稀疏观察与推断区间在页面上有不同视觉语义。

门禁测试：仅 JSONL 历史、双源一致、双源冲突、Reset 前后缺样、历史身份未知。


## 4. P1 高优先级项

### P1-1 全局语义 hash 可能误删真实并发请求

ccusage 在日级聚合时可使用不含 Session 的事件键去重来源副本，但 Codex Meter 是长期事实库，不能把两个 Session 在同一毫秒产生相同模型和 Token 的事件一概视为重复。

处理：文件副本使用 Session + fingerprint；跨 Session 只通过明确 replay 关系抑制。用 ccusage 对账检查是否因此出现差异，并记录差异原因。

### P1-2 service tier 缺失与未知必须区别处理

缺字段表示没有新信息，应继承；字段存在但值未知表示状态发生了不可理解的变化，应清除旧状态。当前单纯 normalize 成 `unknown` 可能丢失这一区别。

处理：原始观察保存 `field_presence`，normalizer 决定继承或清除。

### P1-3 质量不能只保存一个枚举

同一 delta 可能同时 `mixed_account + fast_unknown + missing_samples`。单个字符串会丢失信息。

处理：保存 summary severity，并用关联表或稳定 JSON 数组保存多个 flag；flag 具有 `blocks_pricing`、`blocks_calibration` 等能力属性。

### P1-4 Credit/USD 不应使用 SQLite REAL

浮点聚合、重算和不同语言序列化会制造尾差。

处理：费率和结果使用整数微单位或明确的十进制定点。只有 UI 格式化和 ccusage 浮点对账边界允许转换为浮点。

### P1-5 配额稀疏通知和乱序样本

通知可能稀疏、重复或乱序。直接 upsert 最新行会破坏窗口轨迹。

处理：保留原始通知；只有基于完整快照成功合并后才产生 canonical snapshot；按照服务端/接收时间分别保存，乱序样本不回写当前状态。

### P1-6 账号切换存在通知竞态

JSONL 事件时间、App Server 通知到达时间和采样时间不完全一致。

处理：上下文区间保存 observed/source-effective 两种时间和置信度；明确边界检查点；无法拆分的 delta 进入 Unknown，而不是强行归给切换前后任一账号。

### P1-7 日百分比不能只用首尾相减

同一天可能发生 Reset、账号切换或多个 limit。

处理：按 quota window 分段，计算同窗口正向变化后再汇总；页面显示分段，不制造单一连续曲线。

### P1-8 账号每日 Token 有延迟且日期时区未知

远程 bucket 的 `startDate` 不应直接当成本地自然日事实。

处理：原始日期不变，派生对齐日期带 timezone/freshness；只有 settled 且身份可比时生成 coverage。

### P1-9 Thread 标题存在隐私风险

标题可能由对话内容生成，虽然不是完整正文，仍可能包含敏感信息。

处理：标题保存可配置，默认仅本地；提供隐藏标题模式和重建/清除功能；导出默认不包含标题。

### P1-10 projector 必须支持失效范围

价格版本、Session 关系或人工账号映射发生变化后，不应全库盲目重算，也不能只更新未来数据。

处理：每次修改产生 affected range/session/window，进入 dirty queue；增量结果与全量 rebuild 必须一致。

### P1-11 capacity 估计有方向性偏差

共享账号中分母 `usedPercent_delta` 包含其他机器，本机 Credit 不包含，因此候选容量通常偏低。

处理：共享区间默认 contaminated；页面明确提示偏差方向；只有单机确认区间才能成为正式证据。

### P1-12 本地 HTTP 仍需防 CSRF

仅绑定 loopback 不能阻止恶意网页尝试调用本机写接口。

处理：同源、严格 Origin、无通配 CORS、session/CSRF token、危险操作二次确认。

### P1-13 双源配额不能按时间最近简单覆盖

JSONL 观察时间来自事件，App Server 同时存在服务端状态和本地接收时间。简单选择“最近一条”可能把乱序或稀疏通知当成更权威数据。

处理：按账号、limit、window、reset generation 配对；实时完整读取优先用于当前卡片，JSONL 优先补历史覆盖；冲突由 canonical 选择规则处理并保留双方证据。

### P1-14 ccusage 对账必须检查参数兼容性

不同 timezone、日期过滤、pricing override、speed policy 或 ccusage 版本的结果不能直接比较。

处理：为 run 计算 comparison compatibility key；页面只默认选择兼容结果，不兼容结果仍可查看但明确标记。


### P1-15 可选本地辅助文件不能偷偷变成第三条账本

`state_5.sqlite`、`session_index.jsonl` 和配置文件可能提供标题、路径或设置，但它们不是 Token、配额或 Credit 的主源。如果实现者直接依赖它们推断历史账号或消耗，系统会产生不可追溯的隐式口径。

处理：R0 明确启用/禁用决定；启用时只抽白名单字段并标记 `local_auxiliary`，禁用时保留 Unknown；不允许这些文件覆盖 JSONL/App Server/ccusage 事实。

## 5. P2 优化项

- JSONL 行预过滤可参考 ccusage 的字节 marker 优化，但必须先保证正确性。
- 多 Session 可并行解析，同一 Session normalization 必须有序。
- ccusage 对账频率根据历史扫描耗时自适应退避。
- minute/day/session/window 投影可按 dirty range 合并批次。
- App Server schema 版本变化应记录 compatibility status，不要静默忽略未知字段。
- 来源参考文档发生字段边界修订时，必须生成对应的 schema/test 更新，不允许只改文字说明。
- 价格候选更新可联网发现，但生产版本必须人工确认且不可变。
- 诊断页应显示“事实水位”和“投影水位”，帮助识别页面落后于采集。

## 6. ccusage 参考实现审查结论

本轮源码审查基于本地 commit `5fd1591d3a4abdd63c0256b248157bf1568b57b8`，项目当前 CLI contract 锁定为 `20.0.19`。后续若升级其中任一版本，必须重新运行 fork replay、Token 差分、日期和计价兼容测试。

### 可直接借鉴的行为

- 累计未变化时不重复接受 last usage。
- last 缺失时按累计值逐字段差分。
- 缓存 Token 是 input 子集，输出 reasoning 是 output 子项。
- `default/standard` 和 `priority/fast` 的映射。
- 缺失 tier 字段继承、未知 tier 清除。
- fork parent metadata 的两个路径。
- parent replay prefix 只取 fork 时间之前部分。
- 请求级长上下文 bucket。
- IANA timezone 和 `[day_start, next_day_start)`。
- 日级与 Session 级 JSON 作为独立黄金对账结果。
- 完整历史批量对账和受影响范围低频重跑。

### 不能原样照搬的行为

- ccusage 是一次性报表，可在某些聚合模式使用更激进的全局事件 key；长期事实库需要更保守的去重证据。
- ccusage 的 fork parent 缺失 1 秒 burst 是启发式，Codex Meter 必须保留质量和证据。
- ccusage 浮点 cost 适合展示/验证，不适合本系统长期正式账本。
- ccusage Session 报表按原始 Session ID 分组，不负责产品所需的 conversation group。
- ccusage 不包含本系统需要的官方百分比、Reset、账号上下文和分钟级验证；这些由 JSONL/App Server 双源及内部投影负责。
- ccusage 默认/强制 speed policy 是报表能力；本系统必须对 Unknown tier 表达不确定范围。
- ccusage 在线价格发现不能直接成为历史生产价格。

## 7. 数据模型审查

### 必须具备的不变量

1. 一个原始观察可以被重复读取，但 canonical observation 只能有一份。
2. 一个 replay-safe Token delta 只能进入一个账号上下文。
3. 一个 delta 在每个 pricing scheme 中最多有一个正式计价版本。
4. raw Session 和 conversation root 都不能改变 delta 的总和。
5. minute、day、raw session、conversation 和 window 投影应能回到同一组 delta。
6. 删除所有派生表后可以完全重建。
7. 修改人工关系或容量不能改写原始观察。
8. unknown/null 与真实 0 必须可区分。
9. JSONL/App Server quota 原始观察永不因 canonical 选择而被覆盖。
10. ccusage 旧运行永不被新运行物理覆盖，兼容参数下才允许比较。
11. 每个正式指标和原始字段都有来源参考章节以及 scope 标签。

### 索引和约束要求

- `token_observations(machine_id, session_id, observed_at_ms)`。
- `usage_deltas(machine_id, event_at_ms)`。
- `usage_deltas(session_id, event_at_ms)`。
- `quota_snapshots(account_identity_id, limit_id, observed_at_ms)`。
- `account_context_intervals` 不重叠约束。
- `pricing_versions(scheme, effective_from)` 唯一且区间不重叠。
- `session_relations(child_session_id, version)` 唯一有效关系。
- 金额非负，除非未来明确支持冲正事件。

## 8. 阶段顺序审查

计划阶段顺序合理，关键依赖如下：

```text
R0 工具链/契约
  ↓
R1 事实模型
  ↓
R2 JSONL 正规化
  ↓
R3 ccusage Token 对账与结果持久化
  ↓
R4 App Server
  ↓
R5 归因/计价
  ↓
R6 多粒度投影
  ↓
R7 窗口/Token 参考
  ↓
R8 页面一
  ↓
R9 容量页
  ↓
R10 口径/安全/运维
  ↓
R11 七天验收
```

不允许的提前行为：

- R3 对账未通过前实现正式 Credit 页面。
- R2 历史 rate-limit backfill 未通过前实现历史配额趋势页面。
- R5 归因/计价未通过前实现容量候选。
- R7 Reset/window 未通过前画跨窗口累计曲线。
- R8 页面一真实验收前冻结容量页面交互。
- R10 安全门禁前开放任何修改容量/关系的写接口。

## 9. 端到端追溯矩阵

| 页面指标 | 原始来源 | 核心算法 | 事实/投影 | 主要门禁 |
| --- | --- | --- | --- | --- |
| 每日总 Token | JSONL | delta、replay、dedupe、日期切分 | usage_deltas → daily | 与 ccusage daily 相等 |
| 每分钟 Credit | JSONL | 事件计价、分钟 bucket | usage_deltas → minute | 分钟和日求和一致 |
| Session 成本 | JSONL | raw session 聚合 | session_rollups | 与 ccusage session 相等 |
| 对话组成本 | JSONL +关系 | replay-safe root 聚合 | conversation_rollups | 与 raw session 总和一致 |
| 官方百分比 | JSONL 历史 + App Server 实时 | backfill、双源认证、sparse merge、窗口识别 | quota observations/snapshots/windows | 历史、Reset、冲突测试 |
| 未观测百分比 | JSONL + App Server | 账号变化减本机估算 | window_rollups | 不标成另一台机器精确量 |
| 账号 Token 对照 | App Server usage + JSONL | 日期对齐、freshness | token_reference_daily | pending/incomparable 不计算 |
| 候选容量 | quota + local Credit | 清洁区间、稳健回归 | calibration_results | 阻断 flag 不进入拟合 |
| 内部/ccusage 对照 | JSONL + ccusage | 参数兼容、逐项差异 | persisted reconciliation | 页面可见、可重跑 |
| 算法说明 | 全部元数据 | 数据血缘 | methodology API | 与实际版本一致 |

## 10. 真实验收场景

### 场景 A：Plus 单机账号

- 一整个 Reset 窗口只有本机使用。
- 本机 Credit 累计与官方百分比变化方向一致。
- 可生成高可信容量候选。
- 多个窗口候选离散程度可见。

### 场景 B：Pro 两机共享账号

- 账号百分比变化大于本机估算。
- 页面显示未观测/误差，不显示“另一人精确用量”。
- 默认 contaminated，不自动进入容量确认。

### 场景 C：API/provider 历史

- 显示 Token 和 API 等价美元。
- 不显示订阅窗口占比。
- provider/plan 不明确时保留 Unknown 和人工映射入口。

### 场景 D：fork 与子代理密集对话

- 原始 Session 列表能看到父子关系。
- conversation 视图只有一份 replay 历史。
- 子代理真实新增使用仍计入总量。
- 与 ccusage 的原始 Session/日总量差异可解释。

### 场景 E：同日 Reset、账号和价格边界

- 日期页面分段展示。
- 事件只归一个账号和一个价格版本。
- 日总量等于分段之和。
- 不跨 Reset 连线。

### 场景 F：服务安装前的历史回填

- 只依赖历史 JSONL 恢复 Token、配额百分比观察和 Reset 证据。
- 历史静默区间显示缺口，不伪造连续采样。
- App Server 启动后的重叠时段显示 corroborated 或 mismatch。
- 每个历史日期和原始 Session 都能查看内部/ccusage 对账；无对应粒度时明确显示不可比较。

## 11. 剩余不可消除风险

- 官方百分比可能只有整数精度，容量只能估计区间。
- 另一台机器不可读取，未观测量包含其他机器与模型误差。
- App Server usage bucket 日期语义和结算延迟不完全公开。
- 历史 JSONL 可能缺失 tier、账号或父 Session 文件。
- Codex schema、模型别名、价格和长上下文规则未来可能变化。

这些风险必须通过版本、质量标签和页面措辞表达，不能通过默认值掩盖。

## 12. 最终批准条件

满足以下条件后，计划可以进入 R1 实施：

1. Rust 工具链恢复，所有现有测试可运行。
2. 当前未提交文档被安全保留。
3. 确认是否存在需保留的外部数据库。
4. 页面指标字典和 API 示例完成审阅。
5. ccusage 版本与参考 commit 固定。
6. P0-1 至 P0-10 全部转化为自动测试或阶段门禁。

审查最终结论：**有条件批准**。方向、产品覆盖和数据链路正确；下一步应执行 R0，而不是继续旧计划的 App Server 阶段。
