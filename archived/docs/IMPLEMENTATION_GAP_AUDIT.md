# 《最终设计》实现差距审查

审查对象：`docs/FINAL_DESIGN.md` 与当前工作区代码。

结论先说：当前代码不是《最终设计》的等价实现。现在完成的是一条“JSONL 扫描 → 基础 Token 投影 → SQLite → API → 简单页面”的纵向切片，另外补了 App Server/ccusage 的适配器骨架；而最终设计要求的是“本机账本 + 账号官方窗口 + 双源验证 + 账号上下文归因 + 可确认容量标定 + 真实产品页面 + 长期运行运维”的完整闭环。两者之间存在多处阻断级差距。

## 1. 逐节核对

状态含义：

- 已实现：代码和数据链路基本符合设计，并有针对性测试。
- 部分实现：有表、类型或骨架，但真实数据链路、边界或页面没有闭合。
- 未实现：当前没有满足设计目标的可用实现。
- 方向偏离：代码有同名功能，但计算口径或产品形态不是设计要求的口径。

### 1.1 指标口径（设计第 0 节）

| 设计要求 | 当前情况 | 结论 |
| --- | --- | --- |
| App Server `account/usage/read.dailyUsageBuckets` 作为账号级每日 Token | 解析器和表存在；只有真实 App Server 返回时才会写入。历史烟测没有真实账号快照，当前 UI 也没有完整展示 lifetime、bucket、freshness | 部分实现 |
| JSONL/ccusage 对账使用 `totalTokens` | 字段和差异表存在；实际完整历史 ccusage 对账尚未跑过 | 部分实现 |
| 官方百分比优先来自 App Server，JSONL 历史百分比标记 estimated | 双源观察表和 canonical 选择存在，但页面没有完整区分官方/估算，也没有可靠的 Reset 分段时间线 | 部分实现 |
| Credit、API 美元、账号 Token、本机 Token、官方百分比分栏 | 数据库有若干独立字段；页面只展示少量卡片和表格，账号/本机/未观测的完整语义没有落地 | 部分实现 |

### 1.2 产品边界和总体架构（设计第 1、2 节）

| 设计要求 | 当前情况 | 结论 |
| --- | --- | --- |
| 两人共享账号时区分“账号总窗口”和“本机消耗” | 本机 Token 和账号百分比有各自的表，但 Token delta 没有绑定 `account_context_interval`，因此无法可靠回答某段本机消耗属于哪个账号上下文 | 方向偏离/阻断 |
| Plus 单机、Pro 共享、API/Other、Unknown | 枚举和 App Server 实时身份解析存在；历史 JSONL 没有完成归因，`fixtures/mappings/provider-history.json` 也没有进入投影链路 | 部分实现 |
| 单一后台进程同时负责 JSONL、App Server、ccusage、网页和 API | 后端现在可以托管静态页面；但 App Server/ccusage 默认不运行，调度策略未实现，仍是“适配器骨架 + 手动入口” | 部分实现 |
| 不保存正文、认证秘密 | JSONL 使用白名单，App Server/ccusage 有脱敏逻辑 | 已实现（需继续做安全验收） |

### 1.3 采集策略与频率（设计第 3 节）

#### JSONL

设计要求文件事件监听、2 秒 debounce、启动扫描 active/archived、增量 offset、每 6 小时完整检查。

当前 `JsonlCollector::watch()` 和 debounce 类型存在，但主程序实际使用的是固定 10 秒全目录扫描（`src/main.rs:143-174`），没有接入 watcher，也没有 6 小时完整一致性任务。启动扫描、游标、半行、截断、归档 inode 和白名单字段已经实现。

结论：部分实现，运行调度方向不符合设计。

#### App Server

设计要求长连接通知、启动/重连/账号切换快照、活跃 60 秒/空闲 5 分钟配额兜底、`T-60s/T+15s/T+60s` Reset 采样、每日 Token 每 6 小时/跨日采集、Thread 元数据分页。

当前 supervisor 只发送初始化、账号读取、配额读取、usage 读取和一次 `thread/list`（`src/collectors/app_server_runtime.rs:198-217`），随后按同一个 poll interval 读取配额和 usage（`src/collectors/app_server_runtime.rs:219-234`）。没有空闲状态策略、Reset 前后采样、usage freshness 状态机或 Thread 分页/落库；`thread/tokenUsage/updated` 解析后也没有进入 Session 统计。

结论：部分实现，尚未达到设计采集策略。

#### Token 参考维度

设计要求已结算日期、同账号、无混杂时才计算 coverage，并区分 `pending/stale/settled/incomparable`。

当前只按 bucket 日期和本机日期直接重建，主要质量值是 `reference`、`local_pending` 等，缺少完整的结算延迟、stale 窗口、身份混杂和时间对齐门禁。

结论：部分实现。

#### ccusage

设计要求 JSONL 变化 debounce、session 对账、订阅 Credit 自动/强制 Standard 两套、API 美元独立两套、启动/每日/每 6 小时/账号切换检查点、完整历史对账和边界前缀计算。

当前启动校验命令只有 `codex daily --json --offline`，且只运行 `api_usd_equivalent` 一套（`src/main.rs:177-213`）；页面手工运行也是同一类 daily 命令。虽然结果表和差异表存在，但完整调度、session 命令、双 scheme、价格 override 和价格边界前缀重算都没有完成。

结论：方向偏离/部分实现。

### 1.4 账号上下文和历史归因（设计第 4 节）

设计要求每个 Token delta 都按 `[start_at,end_at)` 连接到唯一账号上下文，账号切换时切开区间，历史 provider 映射可人工修订且有审计。

当前最关键的问题是：`rebuild_usage_deltas()` 查询 Token 事实时没有读取上下文，并在写入时明确写入 `account_context_interval_id = NULL`、`end_at_ms = observed_at_ms + 1`（`src/rollups/usage.rs:70-80`、`src/rollups/usage.rs:214-230`）。JSONL 采集时也把 `context_interval_id` 写成 NULL。这样数据库虽然有账号上下文表，但本机 Token、Credit 和 Session 没有真正归因到账号区间。

结论：阻断级未完成。当前无法可靠实现“谁在什么时候用了多少”以及账号切换前后拆分。

### 1.5 SQLite 数据模型（设计第 5 节）

表结构覆盖面较大，已经有 machines、account identities、JSONL facts、quota observations、usage snapshots、Session/conversation rollups、pricing、capacity、reconciliation 等表。

但以下部分还只是结构或未接通：

- `collector_runs`、`calculation_runs` 没有形成完整的运行记录和 dirty-range 队列。
- `quality_flags` 有写入方法，但主要投影没有统一写入该表。
- `plan_capacities` 与新的 `capacity_versions` 并存，页面写 draft 使用后者，主页没有消费已确认容量形成占比。
- account context、usage delta、minute/session/day rollup 之间的关联没有闭合。

结论：表结构部分实现，数据生命周期未完成。

### 1.6 计算公式（设计第 6 节）

#### Token 和标准计价

非缓存输入、缓存输入、输出、推理不重复收费的基础逻辑已存在；未知模型会标记 `missing_pricing`。但实际本机历史中模型别名/价格卡没有完成覆盖，烟测结果出现大量金额缺失，因此核心 Credit/API 美元不是可用的完整历史账本。

#### Fast

设计要求：

```text
scheme_total = ccusage(auto, scheme)
scheme_standard = ccusage(force-standard, scheme)
fast_surcharge = max(total - standard, 0)
```

当前内部计算直接使用价格表中的固定 Fast multiplier；ccusage 只执行一套 daily API 命令，没有自动/强制 Standard 差分，也没有订阅/API 两套独立对账。

结论：公式方向偏离。

#### 本机套餐占比

设计要求使用已确认容量计算本机窗口/日占比，并在主页显示。当前 window rollup 只保存本机 Token/Credit 汇总和官方首尾百分比，未把 `plan_capacities.status=confirmed` 接入主页占比计算。

结论：未完成。

#### 账号总量和未观测量

设计要求计算账号 usedPercent 变化、本机估算占比和未观测/误差区域。当前有 Token 参考表，但没有完整的“窗口增量账号消耗 − 本机估算窗口消耗”的正式投影和展示。

结论：未完成。

### 1.7 容量标定（设计第 7 节）

设计要求选择账号/Reset 窗口/区间，图上刷选，稳健回归，多样本、百分比量化误差、污染诊断，候选只能人工确认。

当前算法是清洁样本的中位数，范围是候选最小值/最大值，不是稳健回归或量化误差区间（`src/calibration/mod.rs`）。页面只是手工输入开始百分比、结束百分比和 Credit，再保存 draft；没有真实窗口选择、图表刷选、证据面板和 confirmed 操作。

结论：部分实现，产品形态和算法都未达到设计。

### 1.8 价格版本和价格边界（设计第 8 节）

价格表和生效时间表存在，且有旧版本迁移。但：

- `ccusage` 的离线 pricing override 没有作为实际命令参数执行。
- 8 月 1 日 15:00 的前缀差分流程没有实现。
- 一天内如果混合多个价格版本，日汇总只保留一个 `pricing_version` 值，不能完整表达多个事件版本。
- 实际未知模型的 alias/映射不足，真实历史金额仍大量是 `missing_pricing`。

结论：部分实现。

### 1.9 页面信息架构（设计第 9 节）

#### 页面一：本机用量

设计要求月历、三条 0–100% 阶梯线、账号/本机/未观测分层、Reset 垂线、套餐情景切换、日详情 Token/模型/价格/账号区间/配额采样点。

当前页面实际是日期按钮列表 + HTML 表格（`web/src/app.js:60-117`）：

- 不是月历。
- 没有趋势图、阶梯线、Reset 垂线或未观测灰区。
- 日期按钮主要显示 Token 和质量，不显示 Credit、API 美元、占比和账号类型。
- 没有顶部当前账号、套餐、provider、剩余百分比、倒计时、确认容量。
- 日详情没有完整账号上下文区间和历史人工归因。
- 分钟区间没有同日多 Reset 分段。
- Session 表没有真正的 conversation 视图、模型/服务层拆分展示和主 Session 合并解释。

结论：与设计页面差距最大，属于“字段演示壳”，不是目标页面。

#### 页面二：容量标定

当前是 3 个数字输入框、候选计算和 draft 保存。设计要求的 Reset 区间选择、上下两层图、证据面板、污染诊断、候选区间和人工 confirmed 还没有。

结论：部分实现，不能作为实际标定工具使用。

#### 页面三：设置与诊断

设计要求设置/诊断、provider 映射、采集健康、offset、App Server 连接、导出、备份和重新对账。

当前第三页是“算法与数据口径”静态说明，不是设置与诊断页。相关 API 也没有实现。

结论：未实现。

### 1.10 数据质量（设计第 10 节）

部分 API 字段保留了 `quality_flags`、source、freshness 和 pricing quality，但最终设计要求的 `exact/estimated/mixed_account/unknown_provider/boundary_approximate/missing_samples/fast_unknown/account_usage_pending/account_usage_stale/incomparable` 没有统一贯穿所有日汇总、区间和页面。当前质量主要存在于 JSON 字段中，缺少统一质量实体、筛选和诊断页面。

结论：部分实现。

### 1.11 技术选型、运维和安全（设计第 11 节）

| 设计要求 | 当前情况 | 结论 |
| --- | --- | --- |
| React + TypeScript + Vite | 当前是原生 JS + HTML 字符串模板 + CSS，`web/package.json` 没有 Vite/React/TypeScript | 方向偏离 |
| 图表库、阶梯线、刷选、联动 | 没有图表库和图表组件 | 未实现 |
| 静态资源嵌入二进制 | 当前按 `CODEX_METER_WEB_ROOT` 运行时读取文件 | 方向偏离 |
| launchd install/status/uninstall | 当前只有手工 shell service 脚本，没有 launchd plist 或安装/卸载 | 未实现 |
| Origin + session/CSRF token | 当前写接口只要求自定义确认头 `X-Codex-Meter-Write: confirm` 和 loopback Origin；没有 token 化 CSRF | 未完成 |
| 仅监听 loopback | 后端默认 loopback，服务脚本使用 loopback | 已实现 |

## 2. 当前真正已经完成的部分

不能把当前代码说成“什么都没有”。以下部分确实有价值：

1. JSONL active/archived 扫描、游标、半行、截断、inode/归档和白名单事实。
2. last/cumulative Token 证据、重复通知、counter reset、fork replay 的基础测试和投影。
3. SQLite migration、WAL、原始事实和若干可重建 rollup。
4. App Server JSON-RPC 行解析、身份 HMAC/脱敏、配额/usage 原始落库的适配器。
5. ccusage JSON 结果/逐项差异的持久化结构和无 shell 执行入口。
6. 基础 API、同源 Web 壳、三页导航和健康检查。

这些是“底层原型/第一条纵向切片”，不是最终设计中可用的两个业务页面和完整服务。

## 3. 阻断级差距排序

### P0：不修就不能声称核心目标成立

1. 将 JSONL Token delta 按事件时间关联到 account context；处理历史 provider 映射、账号切换和 mixed account。
2. 重做官方配额 canonical timeline：账号维度、limit/window 维度、Reset 分段、carry-forward/stale、同日多 Reset。
3. 完成 ccusage 全量历史 daily/session 对账，以及 subscription auto/standard、API auto/standard 四套结果。
4. 重做页面一，先实现账号总窗口、本机消耗、未观测/误差、确认容量和 Reset 分段图，再补日/分钟/Session 详情。
5. 让已确认容量真正参与本机窗口/日占比，且未知价格不能以零金额参与正式结果。

### P1：不修就不能按设计使用

6. 页面二改为真实窗口选择、图表刷选、稳健拟合、污染诊断、误差区间和人工 confirmed。
7. 页面三改为设置/诊断/导出/备份/重算，而不是只显示方法说明。
8. App Server 完成 Thread 元数据分页、usage freshness、Reset 采样、活跃/空闲策略和真实登录验收。
9. JSONL 改为事件监听 + debounce + 6 小时完整一致性检查，而不是固定 10 秒全目录扫描。

### P2：上线前必须收口

10. React/TypeScript/Vite 或明确重新冻结前端技术选型；补图表、响应式和 UI 场景测试。
11. token 化 CSRF、严格 Origin/CORS、导出隐私门禁、launchd、日志轮转、健康指标和七天连续运行。
12. 完整真实历史 backfill、真实 App Server/ccusage 双源验证、Plus/Pro/账号切换/Fast/Reset 验收。

## 4. 审查结论

当前不应再称为“已经按最终设计实现”。准确说法应是：

> 已完成底层事实采集、基础归一化、部分投影、API 原型和 Web 壳；核心账号归因、官方窗口语义、Fast/ccusage 双方案、最终页面和生产运维仍未完成。

后续实现顺序必须从 P0 开始，不能继续在当前页面壳上堆字段或继续扩展边缘 API。

## 5. 参考页面 `codex-usage-report.html` 对照（2026-08-04）

这次把 `/Users/Lendfating/Desktop/codex-usage-report.html` 作为页面一的实际验收基线重新核对。该文件是一个自包含的离线报告，不是最终设计的全部（它没有 App Server 实时控制面、分钟官方百分比和容量标定实验室），但它明确展示了页面一应该达到的字段密度、信息层次和可读性。报告内嵌数据的规模为：39 个日志文件、7,647 条真实用量事件、154 条重复事件、18 个根会话、796 条子会话归并事件、17 个有数据日期、18 个根会话和 13 个模型维度。

| 参考页面实际能力 | 当前页面实际能力 | 差距 |
| --- | --- | --- |
| 页头标题、生成时间/时区、本机离线标识，以及 Plus/Codex 5X/Codex Pro 20X 周窗口选择 | 页头只有 Codex Meter、API 健康状态和导航；套餐选择只在容量页的手工表单里 | 缺少当前账号/套餐/窗口语义，入口位置也不对 |
| 30 天活动日历，20 格分页；每格按总 Token 着色，并显示日期、总 Token、美元、登录 Token、周窗百分比 | 自动把 API 返回日期渲染成一组同样的按钮，只显示日期、Token 和质量文字 | 没有月历/分页/热度、美元、登录/其他、窗口占比 |
| 趋势图同时画总 Token、推理、输出、美元，并标出选中日期 | 没有任何趋势图或选中日期标记 | 页面一的主要分析入口缺失 |
| 10 张日卡：总 Token、周窗占比、登录、其他、输入、缓存读、缓存写、输出、推理、API 等价美元 | 只有日期详情里的 Token、Credit、API 美元、官方窗口、账号 Token 五张卡 | Token 分类、登录/其他和本机周窗计算没有完整呈现 |
| 日期详情先给总计表，再可展开登录/其他 | 日期详情有基础卡片和表格，但没有同等的总计/分类展开结构 | 分类证据和比较路径不完整 |
| 当天模型维度：模型、推理强度、服务层、请求数、全部 Token 分类、美元、占比 | 没有页面一的模型维度表 | 无法回答“哪种模型/推理/服务层花了多少” |
| 当天 Session 表：标题、登录/其他及证据、首末时间、主模型/推理、Top3 模型、全部 Token 分类、美元 | Session 表只有名称、Token、Credit、关系、是否有 ccusage | 缺少模型组合、分类证据、时间、价格拆分、占比和真实 ccusage 行 |
| 全局模型维度页签 | 当前模型信息没有独立页签 | 缺少跨日期模型分析 |
| 报告底部显示文件数、事件数、去重数、根/子会话、fork 排除、账号套餐事件/其他事件，并能展开统计口径 | 这些信息散落在 API/方法页，页面一没有审计摘要 | 用户不能在结果旁边判断覆盖范围和可信度 |
| 静态报告即使没有后台服务也能打开、查看历史结果 | 当前页面依赖运行中的 `/api/v1` 和 SQLite；服务/数据未启动时只能看到空壳或错误提示 | 缺少历史回填后的稳定结果展示和离线导出/快照 |

因此，当前页面不是“参考页面的另一种皮肤”，而是少了日历分析、趋势分析、模型维度和 Session 证据四个核心层级。最终设计还要求在这套基础上增加账号官方窗口、Reset 垂线、本机/账号/未观测三层、双源配额和 JSONL/ccusage 对账；这些更没有闭合。

## 6. 为什么执行过程中偏离

1. 实施顺序反了。执行时先扩展 migration、collector、API 和字段壳，页面只作为最后的演示；而最终设计要求先冻结页面数据契约，再反推事实、归因和投影。
2. 把“有类型/有表/有适配器/能返回 JSON”误判成“功能已完成”。例如 `account_context_interval_id` 有列不等于 JSONL delta 已正确归因，ccusage 有命令入口不等于四套历史对账已经跑完。
3. 没有把参考 HTML 设为 golden page。没有用同一份历史样本生成期望的日历、卡片、模型表、Session 表和审计数字，也没有做逐屏/逐字段验收。
4. 没有先验证真实历史回填链路。App Server 不能回溯历史，而本机 JSONL 的 rate-limit/plan/credits 是页面一的关键事实；实现只完成了部分抽取和基础投影，未以“历史百分比趋势必须可见”为门禁。
5. 没有把“生产结果”和“独立验证结果”分成用户可见的两层。ccusage 的保存结构存在，但页面主要显示“有没有 run”，没有做逐日、逐 Session、逐字段对比。
6. 为了尽快得到可启动的服务，加入了原生 JS 页面和 service script。这解决了启动体验，却进一步掩盖了 React/图表/真实产品页面尚未实现的事实。

## 7. 修复原则

不要在当前页面上继续堆卡片。保留可靠的 JSONL 游标、白名单、Token 证据、SQLite WAL、价格基础测试和脱敏执行器；重写/删除误导性的业务投影、配额时间线、页面和容量输入壳。新的完成门禁必须是：

1. 固定参考 HTML + `FINAL_DESIGN.md` 的页面一契约和 golden fixture；
2. 先完成账号上下文归因、历史 JSONL quota timeline、Reset 分段和四套 ccusage 对账；
3. 用同一 fixture 生成 API 期望值，页面逐项显示 JSONL 内部结果与 ccusage 验证结果；
4. 通过页面一的字段/视觉验收后，才实现容量实验室和设置/诊断页；
5. 最后再收口 watcher、App Server 长连接、launchd、CSRF 和七天运行验收。
