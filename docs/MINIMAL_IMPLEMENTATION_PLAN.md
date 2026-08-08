# Codex Meter 最小实现修改计划

## 0. 文档定位

本文是后续实现的唯一执行计划。目标是在当前代码基础上，用最少的模块、表、接口和页面，尽快完成 Codex Meter 的两个核心用途：

1. 看清本机每天、每分钟、每个 Session 用了多少 Token、Credit、API 等价美元，以及账号官方百分比如何变化；
2. 使用真实 Reset 窗口估算并人工确认 20/100/200 美元档位的大致周容量。

`FINAL_DESIGN.md` 保留为产品语义依据；旧 `FINAL_EXECUTION_PLAN.md` 和 `FINAL_EXECUTION_PLAN_REVIEW.md` 保留为历史研究，不再作为实施清单。旧计划中的工程化扩展项只有在用户以后明确要求时才重新进入范围。

三个页面的完整指标、来源职责和八表字段以 [最小页面指标与八表数据模型](MINIMAL_DATA_MODEL.md) 为 M0 冻结契约。第一批来源采集的频率、去重和失败口径以 [第一批来源 Pipeline](MINIMAL_SOURCE_PIPELINE.md) 为准。后续实现不得回到旧设计的多层表结构，也不得加入该文档没有页面消费者的字段。

本计划只规划修改，不授权提交、推送、发布或删除用户数据。

## 1. 当前现实

当前工作区已经具备一些可靠基础，但规模明显超过这个本地小工具的需要：

- `src/`、`web/`、`migrations/`、`scripts/` 合计约 1.5 万行；
- SQLite 当前有约 48 张表；
- `src/api/server.rs` 约 2300 行，`src/storage/repositories.rs` 约 1400 行；
- JSONL、App Server、ccusage 三个采集模块合计约 3500 行；
- API 已扩展到十多个路由，但页面仍没有达到参考报告的核心效果；
- 本机运行库中约 9292 条 `usage_deltas` 全部没有账号上下文；
- 当前数据库中 ccusage 日级/Session 结果为 0，App Server 账号 bucket 和账号上下文也为 0；
- 现有 54 个 Rust 测试通过，说明 JSONL 差分、去重、基础计价和部分解析代码值得保留；
- 当前 Web 只是日期按钮和表格壳，没有参考页面的日历、趋势、模型和完整 Session 信息。

结论：不推倒可靠的 JSONL 正确性代码，但要删除没有形成用户价值的通用框架和派生层，把系统收缩为一条直接的数据链。

## 2. 第一版只保留的产品范围

### 2.1 页面一：用量报告

参考 `/Users/Lendfating/Desktop/codex-usage-report.html` 的结构，实现：

- 顶部：当前账号类型、套餐、provider、官方已用/剩余、Reset 时间；
- 30 天日期热度格：Token、Credit、API 美元、官方百分比变化；
- 两个简单 SVG 图：本机 Token/金额趋势；官方百分比与本机估算百分比趋势；
- 当日卡片：输入、缓存读、输出、推理、总 Token、Credit、API 美元；
- 当日分钟表/图：Token、Credit、API 美元、观察到的官方百分比；
- 当日模型汇总；
- 当日 Session 汇总：标题、主/子/fork、模型、Fast、Token、Credit、美元；
- 两类可见对账：JSONL vs ccusage；JSONL quota vs App Server quota；
- 账号每日 Token 只作为参考行，不参与 Credit 或容量计算。
- 页脚显示扫描文件数、事件数、去重数、Session 数和警告，便于判断覆盖范围。

### 2.2 页面二：容量估算

只实现真正有用的最小流程：

- 列出已经识别的 Reset 窗口；
- 每行显示官方百分比变化、本机 Credit、Token、缺口和是否共享账号；
- 用户勾选“这段时间只有本机使用”；
- 使用 `本机 Credit / 百分比变化` 计算候选容量；
- 多个有效窗口显示候选值中位数和范围；
- 用户手工保存 20/100/200 三档确认值及生效时间。

前端保留最近 30 天双端点滑块，并允许在同一 Reset 窗口内展示“自动估算建议值”入口；只使用已有 report 字段，不新增采集、接口或后端估算服务。数据不足时明确显示“待补数据”，不生成伪结果。正式套餐容量仍需人工确认，自动建议不会覆盖确认值。

### 2.3 页面三：计算说明

保留一个轻量页面，展示实际运行中的：

- Token 总量和缓存口径；
- Credit、Fast、API 美元公式；
- 当前价格表和生效时间；
- JSONL、App Server、ccusage 的职责；
- 当前已知缺口和质量说明。

不做设置中心、诊断平台、导出中心、备份页面或通用数据血缘浏览器。

## 3. 明确不做

第一版明确删除或停止实现：

- React、TypeScript、Vite 和图表库；
- launchd 安装器、日志轮转、七天连续运行门禁；
- 云同步、多机合并和远程访问；
- 通用 dirty-range/projector 框架；
- 48 张表的事实层/业务层/投影层/审计层分层；
- 通用 `quality_flags` 关系表和 calculation run 水位系统；
- Thread 全量分页、`thread/read` 和 Thread Token 实时账本；
- App Server 全协议 schema 镜像；
- 通用人工注释、关系版本和审计平台；
- 自动联网更新价格；
- 自动认定套餐容量；
- 页面写接口之外的复杂 CSRF/session 框架；
- 任何没有直接出现在三个页面上的 API 和数据库字段。

安全底线仍保留：只监听 `127.0.0.1`、严格 Origin、启动时随机写入 token、不保存认证秘密和对话正文。

## 4. 最小技术结构

继续使用现有 Rust + SQLite，避免重换语言造成额外风险。前端改成一个自包含 HTML 文件，不引入前端构建工具。

```text
JSONL ───────┐
App Server ──┼─> SQLite 三张来源表 ─> report builder ─> 四张结果表 ─> /api/report ─> 单 HTML
ccusage ─────┘
```

建议最终代码结构控制在：

```text
src/
  main.rs          # 启动、定时刷新、HTTP
  db.rs            # 小型 schema 和查询
  jsonl.rs         # 复用现有解析/差分/去重
  app_server.rs    # 只实现 account/rateLimits/usage
  ccusage.rs       # 调用、解析、保存校验结果
  report.rs        # 刷新日/分钟/Turn 结果；模型/窗口查询时聚合
  pricing.rs       # 两套价格和容量公式
web/
  index.html       # HTML + CSS + JS + SVG
migrations/
  0001.sql         # 单一最小 schema
scripts/
  codex-meter-service.sh
```

可以为了代码可读性保留少量子模块，但不得再次扩展成多层框架。目标不是机械追求单文件，而是每个文件都直接服务最终页面。

验收目标：核心源码（不含 fixture、文档和生成物）控制在约 7000 行以内；如果超过，必须逐项说明为什么无法合并或删除。

## 5. 最小数据库

SQLite 固定只保留三张来源表、四张页面结果表和一张人工容量表。模型从 Turn 结果聚合；Reset 窗口结果物化到独立结果表：

| 表 | 内容 |
| --- | --- |
| `source_jsonl` | JSONL 的 session/turn/usage/quota 白名单事实；不保存原始行或正文 |
| `source_app_server` | App Server 的 account/quota/usage 精简状态快照 |
| `source_ccusage` | ccusage 日级/Session 级、subscription/API、auto/standard 校验结果 |
| `usage_daily` | 日历、日趋势和账号 Token 参考所需的日期结果 |
| `usage_minute` | 分钟增量、官方采样和 Reset `window_id` 时间轴 |
| `usage_window` | 每个 primary/secondary Reset 窗口的物化汇总 |
| `usage_session` | 每 Turn 一行；页面按 root Session 合并 child/fork |
| `capacities` | 20/100/200 人工确认值、账号映射和生效时间 |

字段和粒度详见 `MINIMAL_DATA_MODEL.md`。价格与公式保存在随代码发布的版本化静态配置中；文件游标保存在可删除、可重建的 sidecar 状态文件中。质量信息只使用每行一个紧凑字段，不建立通用质量实体和关系表。

当前 `.runtime/codex-meter.sqlite` 是可从 JSONL 重建的派生数据库。真正修改 schema 前先生成一份本地备份并确认没有唯一人工数据；确认后才允许建立新 baseline。该检查是唯一的数据迁移门禁。

## 6. 最小数据规则

### 6.1 JSONL

- 扫描 active 和 archived；
- 保留现有 last/cumulative 差分、重复通知、counter reset 和 fork replay 正确性；
- JSONL 来源行只保存实际观察到的 `plan_type/provider/model/tier/session/root/turn`；跨源 `account_key/auth_kind/capacity_profile` 归一化后写入四张结果表，不建立复杂账号上下文图；
- 历史 quota 变化写入 `source_jsonl(kind=quota)`；
- 启动完整扫描，此后按文件 mtime/offset 增量读取；不实现 dirty-range 队列。

### 6.2 App Server

只保留三个调用：

- `account/read`；
- `account/rateLimits/read`；
- `account/usage/read`。

启动时读取；配额每 60 秒读取；账号日 Token 每 6 小时读取。第一版不读取 Thread 列表和正文，不实现完整通知兼容层。App Server 失败时页面继续显示 JSONL 历史，并标记实时状态不可用。

### 6.3 ccusage

ccusage 仍然只是验证器。启动、手工刷新或 JSONL 发生变化后低频执行：

- daily；
- session；
- subscription/API 两套 pricing；
- auto/standard 两种 speed。

所有组合复用同一个命令函数，归一化结果写入 `source_ccusage`。不保存 stdout 原文，不建立多张 ccusage 结果表，也不做复杂 supersede/compatibility 框架；页面默认显示最新成功结果和版本。

### 6.4 计价与百分比

- 价格放在一个小型静态配置中，按事件时间选择；
- 未知模型或未知 Fast 返回空值/范围，不返回 0；
- 本机窗口占比只使用人工确认容量；
- 官方百分比来自 quota sample；历史优先 JSONL，当前优先 App Server；
- 同一 `account_key + limit + resets_at` 视为一个窗口；usedPercent 明显回落时开始新窗口；
- 未观测百分比只显示为“账号变化 - 本机估算”，不归因到另一个人。

### 6.5 第一批来源 Pipeline

- 三个来源只写三张 `source_*` 表；日、分钟、Turn/Session 结果留给第二批 Pipeline。
- JSONL 启动全量、之后默认每 10 秒增量扫描；App Server 配额 60 秒、账号和账号日 Token 6 小时；ccusage 只在启动/手工刷新且低频运行。
- 采集失败独立记录状态，不能用零值补齐；重复状态使用稳定 `source_key` 合并。
- 详细采集边界和代码落点见 `MINIMAL_SOURCE_PIPELINE.md`。

## 7. 最小 API

只保留四个接口：

```text
GET  /api/health
GET  /api/report?date=YYYY-MM-DD
POST /api/refresh
POST /api/capacities
```

`/api/report` 一次返回页面需要的全部聚合：

```text
current
days[]
selected_day
minutes[]
models[]
sessions[]
quota_windows[]
validation
capacities
methodology
```

M0 的字段消费者和八表落点以 `MINIMAL_DATA_MODEL.md` 为冻结契约。API 只负责把八表聚合成上述页面结构；字段名不得因实现方便继续扩张，没有页面消费者的字段不得加入。Turn 作为 `sessions[]` 的可展开明细返回，Reset 窗口直接读取 `usage_window`，不增加新路由。

不再为每张表和每种粒度建立单独路由。

## 8. 现有代码处理清单

### 8.1 保留并复用

- `src/collectors/jsonl.rs` 中已验证的文件游标、半行、截断、active/archived 和解析逻辑；
- `src/normalization/token_delta.rs`、`replay.rs`、`session_graph.rs` 中的正确性算法和测试；
- `src/pricing/calculator.rs` 的缓存/输出/Fast 基础公式；
- SQLite 连接、WAL 和 migration 启动方式；
- `scripts/codex-meter-service.sh`；
- 两个 JSONL fixture、ccusage contract 和少量相关 App Server 样本。

### 8.2 合并或重写

- `src/main.rs`：改成单一刷新循环和四个 API；
- `src/api/server.rs`：从约 2300 行收缩为薄 HTTP 层；
- `src/storage/repositories.rs`：收缩为最小表的直接查询；
- `src/collectors/app_server.rs` 与 runtime：只保留三个账号方法；
- `src/collectors/ccusage.rs`：只保留命令、sanitization、最新结果保存；
- `src/pipelines/result/materialize.rs`：从三张来源表重建四张结果表；`src/service/report.rs` 只读结果表；
- `web/index.html`、`web/src/app.js`、`web/src/style.css`：合并为一个自包含页面，直接参考现有报告。

### 8.3 候选删除

实施时先确认没有唯一数据或仍被编译引用，再删除：

- migration `0001`—`0010` 的多层 schema，替换为一个新 baseline；
- `src/api/contracts.rs`；
- `src/calibration/` 的通用拟合壳；
- `src/reconciliation/` 的通用差异框架；
- 独立 minute/session/conversation/window/reference rollup 文件；
- 空 scaffold 目录和 `.gitkeep`；
- 与三个账号方法无关的 App Server 全量 schema fixture；
- 旧 Web scaffold 和无实际产品价值的测试；
- 计划执行过程中确认没有页面消费者的 API、表、类型和过期实现状态文档入口。

旧设计文档保留，不作为运行时代码依赖。

## 9. 实施顺序与门禁

### M0：冻结最小契约

只做：

- 以参考 HTML 固定 `/api/report` 示例；
- 固定三个页面字段；
- 备份当前 SQLite，确认可重建；
- 建立“保留/删除”文件清单。

门禁：每个保留字段都能直接映射到页面；任何没有页面消费者的字段从第一版删除。

### M1：最小事实库和 report builder

只做：

- 建立 8 张表的新 baseline；
- 接入现有 JSONL 扫描和正确性算法；
- 在第二批 Pipeline 物化日、分钟、Reset 窗口和 Turn/Session 结果，`report.rs` 只读这些结果；
- 接入静态价格和人工容量。

门禁：参考 fixture 的 Token、模型、Session 和日期结果与参考报告/ccusage 一致；所有未知金额保持空值。

### M2：先完成页面一的 JSONL 主体

只做：

- 单 HTML 页面；
- 日历、趋势、卡片、模型、Session 和分钟详情；
- JSONL 与 ccusage 并排对照。

门禁：页面一在真实历史上达到参考 HTML 的信息完整度；缺失 App Server 时仍然可用。

### M3：补 App Server 官方信息

只做：

- 三个 App Server 调用；
- 当前账号/套餐；
- 官方百分比、Reset；
- 账号每日 Token 参考；
- JSONL/App Server quota 并排对照。

门禁：App Server 断线不影响本机报告；当前官方值和历史 JSONL 值来源清晰，不互相覆盖。

### M4：容量页和公式页

只做：

- Reset 窗口列表和简单候选公式；
- 多窗口中位数/范围；
- 人工保存 20/100/200；
- 实际配置驱动的计算说明。

门禁：共享账号默认不可作为干净样本；候选不能自动覆盖人工确认值；确认容量能返回页面一计算本机占比。

### M5：删除冗余并验收

只做：

- 删除未使用的表、路由、类型、fixture、scaffold 和文档入口；
- 保留必要正确性测试并增加一个完整 report 快照测试；
- 验证 start/restart/stop 和 Web 访问；
- 在真实历史上人工检查三个页面。

门禁：无未使用模块；无页面消费者的 API 为 0；目标表固定为 8 张（过渡期允许旧归档表保留）；核心源码显著少于当前规模；所有保留测试通过。

## 10. 完成定义

只有同时满足以下条件才算完成：

1. 一个命令启动后即可打开 Web；
2. 页面一能展示日、分钟、模型、Session、Token、Credit、美元和官方百分比；
3. 页面能直接看到 JSONL vs ccusage、JSONL quota vs App Server 两组对照；
4. 历史百分比和 Reset 在没有 App Server 历史时仍由 JSONL 展示；
5. 页面二区分干净/共享窗口并给出透明的容量候选；
6. 页面三解释实际公式和价格；
7. 未知值不显示成 0，本机值不冒充账号总值；
8. 数据库不保存对话正文、认证 token 或 API Key；
9. 代码、表和 API 都能找到直接页面消费者；
10. 没有执行用户未批准的额外功能。

## 11. 执行纪律

- 严格按 M0 → M5 执行；当前阶段门禁未通过不得进入下一阶段；
- 每次开始前列出计划条目、允许修改文件、非目标和验收命令；
- 发现需要扩大范围时停止并请求确认；
- 不允许以“以后可能需要”为由保留通用框架；
- 不允许把骨架、表或 API 存在描述为完成；
- 不提交、不推送，除非用户明确要求。

## 12. 2026-08-07 当前执行记录

本节是本轮已执行内容的证据记录，覆盖范围严格限定为第一批 root 修复和第二批结果 Pipeline；前端、App Server 采集协议、ccusage 命令与旧归档均未改动。

| 计划任务 | 实现文件 | 测试/真实验收 | 页面或 API 验收 | 当前状态 |
| --- | --- | --- | --- | --- |
| M1 第一批：写入最终 root_session_id | `src/pipelines/source/jsonl.rs`, `src/db.rs` | `cargo test`；父链/环检测；真实运行库 14,494 条 source 行无未解析 root，未发现把中间父节点当 root | `usage_session.root_session_id` 直接使用来源最终根 | 已验证 |
| M1 第二批：去重、跨日/Reset Turn 拆分、reasoning_effort、mixed_plan、官方百分比 | `src/pipelines/result/materialize.rs` | `cargo test` 35/35；dedupe、跨日 Turn、Reset/官方百分比回归测试 | `usage_daily`、`usage_minute`、`usage_session` 字段可读 | 已验证 |
| M1 第二批：物化 Reset/周窗口 | `config/schema.sql`, `src/db.rs`, `src/pipelines/result/materialize.rs`, `src/service/report.rs` | 真实运行库八表；`usage_window=1`、`usage_daily=20`、`usage_minute=3695`、`usage_session=1530`；`PRAGMA integrity_check=ok` | `/api/report.quota_windows` 直接读取 `usage_window` | 已验证 |

## 12. 精审结论

本计划已经按“是否直接支撑最终页面”重新审查：

- Rust、SQLite、JSONL 正确性算法和服务脚本已有可复用价值，保留比换语言重写更快；
- 只持久化页面直接消费的日、分钟和 Turn 三种结果；模型和 Reset 窗口查询时聚合，不再建立更多持久化框架；
- App Server 只需账号、配额和账号日 Token，Thread 控制面不属于核心目标；
- ccusage 需要保存并可见，但一张 `source_ccusage` 表和紧凑模型拆分足够，不需要多层 reconciliation schema；
- 容量估算前端第一版使用真实窗口公式扫描已有同 Reset 区间，展示最大有效候选及样本区间；不自动确认、不覆盖人工值，复杂统计只有实际数据证明需要时再增加；
- 单 HTML、简单 SVG 和四个 API 足以完成三个页面，不需要前端工程体系。

因此，这是一份“复用正确代码、重写用户可见主链、删除通用框架”的收缩计划，不是对旧大计划的继续扩展。
