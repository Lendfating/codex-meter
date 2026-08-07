# Codex Meter 最小实现执行状态

本文件只记录 `docs/MINIMAL_IMPLEMENTATION_PLAN.md` 的执行证据，不扩展产品范围。

## M0：冻结最小契约 — 已验证

| 计划任务 | 实现/记录 | 测试或证据 | 状态 |
| --- | --- | --- | --- |
| 固定 `/api/report` 字段 | `docs/MINIMAL_IMPLEMENTATION_PLAN.md` 第 7 节 | 字段只覆盖三个页面：当前账号/官方窗口、日/分钟/模型/Session、容量和公式说明 | 已验证 |
| 检查现有 SQLite 可重建 | 当前 `.runtime/codex-meter.sqlite` | `sqlite3 .runtime/codex-meter.sqlite 'PRAGMA integrity_check;'` → `ok`；人工容量、注释、账号上下文、ccusage 结果均为 0 | 已验证 |
| 保留本地备份 | `.runtime/codex-meter.sqlite.m0-backup-20260805` | 备份 `PRAGMA integrity_check` → `ok`；关键计数：`token_observations=9513`、`usage_deltas=9309`、`quota_observations=9513`、`daily_rollups=18` | 已验证 |
| 冻结保留/删除边界 | 下表 | 与最小计划第 8 节一致；未删除任何现有用户文件 | 已验证 |

### 保留边界

- 保留：JSONL 文件游标、active/archived 扫描、差分/去重/replay 算法及测试；SQLite/WAL；静态计价；三个 App Server 账号接口；ccusage daily/session 验证；单 HTML 和服务脚本。
- 保留目标：最终只保留计划规定的 7 张小表；日、分钟、模型、Session、Reset 窗口由 report builder 聚合，不各自建表。
- 候选删除：多层迁移、通用 reconciliation/calibration、独立 rollup、全量 App Server schema fixture、无页面消费者的路由/类型/测试。删除前必须确认没有编译引用，也不得覆盖唯一人工数据。
- 当前不删除：现有迁移、数据库、旧文档和工作区已有改动；它们仍用于 M1 前的对照和回滚。

## 工作区保护

开始 M0 时工作区已有修改和未跟踪文件；本阶段未覆盖、重写或删除这些内容，也没有提交、推送或发布。`.runtime/codex-meter.sqlite.m0-backup-20260805` 是本阶段唯一新增的运行时备份。

## M0 收口

M0 门禁已通过；M1 已按下方记录完成。旧实现仍暂时编译以便回归，尚未接到启动入口，属于 M2 的收口工作。

## M1：最小事实库和 report builder — 已验证

| 计划任务 | 实现文件 | 测试/fixture | 页面或 API 验收 | 状态 |
| --- | --- | --- | --- | --- |
| 7 张事实表 baseline | `migrations/0001.sql`, `src/minimal/db.rs` | 内存 SQLite 用户表计数为 7；SQLite/WAL、唯一摘要和文件游标可写 | 日/分钟/模型/Session/窗口均从事实表聚合，不新增派生表 | 已验证 |
| JSONL 主链 | `src/minimal/jsonl.rs` | `cargo test --offline minimal::jsonl::`；Plus/Pro fixture 首次写入、第二次扫描无新事件 | active/archived、历史文件游标、重复事件和 quota sample 已落库 | 已验证 |
| Token 差分与日期分组 | `src/minimal/report.rs` | report fixture snapshot：2026-07-23 总 Token `503074`，2026-08-04 总 Token `17708` | `days[]`、`selected_day.minutes/models/sessions` 已生成 | 已验证 |
| 静态计价 | `src/minimal/pricing.rs` | 未知模型/未知 Fast 不返回零；模型 Fast 倍率单测通过 | 金额未知显示 `NULL`，不伪装成已计价 | 已验证 |

M1 验收命令：`cargo test --offline minimal::`（5 tests passed）；完整旧测试回归：`cargo test --offline`（58 library + 1 binary tests passed）。

## M1 收口时的下一阶段

只能进入 M2：把最小 report 接到四个 API 和单 HTML 页面，先完成 JSONL 主体和页面一的参考结构；不添加 App Server、ccusage 调度或容量实验室功能。

## M2：页面一和最小 API — 已验证

| 计划任务 | 实现文件 | 测试/证据 | 页面或 API 验收 | 状态 |
| --- | --- | --- | --- | --- |
| 四个最小接口 | `src/minimal/server.rs`, `src/main.rs` | `cargo test --offline minimal::server::`；临时回环服务 `curl /api/health` 返回 `schema=minimal-r1,tables=7` | `/api/health`、`/api/report`、`/api/refresh`、`/api/capacities` 已固定 | 已验证 |
| 单 HTML 页面 | `web/index.html` | 临时 fixture 服务返回 HTTP 200，浏览器 DOM 检查无 console 阻断 | 日历热度、趋势、日卡片、分钟、模型、Session、双源对账、审计 footer 均可见 | 已验证 |
| 参考页面信息密度 | `web/index.html` | 浏览器 snapshot：Plus 15%/85%、Reset、17,708 Token、分钟/模型/Session、JSONL quota 均出现；无 App Server/ccusage 时显示待定 | 单页面不再依赖旧 `web/src/*`，未知金额显示待定而不是 0 | 已验证 |

M2 验收命令：`git diff --check`、`cargo test --offline minimal::`（6 tests passed）。临时服务使用 `/tmp` 数据库和 fixture，已停止；未修改现有 `.runtime/codex-meter.sqlite`。

## 当前下一阶段

M2 门禁已通过。下一步只能进入 M3：接入 App Server 的 `account/read`、`account/rateLimits/read`、`account/usage/read`，让当前账号/官方窗口/账号每日 Token 与 JSONL 并排显示；不读取 Thread 列表和正文。

## M3：App Server 官方账号/配额/每日 Token — 已验证

| 计划任务 | 实现文件 | 测试/fixture | 页面或 API 验收 | 状态 |
| --- | --- | --- | --- | --- |
| 三个账号接口 | `src/minimal/app_server.rs`, `src/main.rs` | `minimal::app_server` fixture 行测试覆盖 account/read、rateLimits/read、usage/read；只发送三类请求 | `current.account`、`current.official` 和 `current.account_daily_tokens` 由 App Server 优先 | 已验证 |
| 认证数据清理 | `src/minimal/app_server.rs` | 测试断言落库 JSON 不含 `access_token`；账号 key 仅保存 hash | 页面只显示套餐/provider，不显示秘密或原始邮箱 | 已验证 |
| JSONL/App Server 并排职责 | `src/minimal/report.rs`, `web/index.html` | report 测试断言 `official.source=app_server`；无 App Server 时 M2 fixture 仍显示 JSONL/待定 | quota 对账表和账号每日 Token 参考表可见；账号 Token 不参与本机 Credit/容量 | 已验证 |

M3 验收命令：`cargo test --offline minimal::`（7 tests passed）。App Server 实际进程轮询仅在 `CODEX_METER_APP_SERVER_ON_BOOT=1` 时启用，失败不会阻断 JSONL 历史服务；未读取 Thread 列表或正文。

## 当前下一阶段

M3 门禁已通过。下一步只能进入 M4：把真实 Reset 窗口接到最小容量公式，保存 20/100/200 人工确认值，并把页面三的公式说明改成实际配置；不引入自动容量拟合或设置中心。

## M4：容量估算与公式说明页 — 已验证

| 计划任务 | 实现文件 | 测试/fixture | 页面或 API 验收 | 状态 |
| --- | --- | --- | --- | --- |
| Reset 窗口候选 | `src/minimal/report.rs`, `web/index.html` | `capacity_candidate(20,10)=200`；零变化/零 Credit 返回 `None` | 页面二仅对勾选“仅本机”的窗口计算中位数/范围，共享或缺失样本不自动纳入 | 已验证 |
| 人工保存 20/100/200 | `src/minimal/server.rs`, baseline `capacities` 表 | API 单测 POST `usd100` confirmed 成功，计划码/金额校验生效 | 页面二保存确认值；候选不会自动覆盖确认值 | 已验证 |
| 公式/价格说明 | `src/minimal/pricing.rs`, `src/minimal/report.rs`, `web/index.html` | report 返回 `methodology.price_card`；inline JS 语法检查通过 | 页面三显示实际 pricing version、模型输入价格、Fast 倍率、来源职责和 NULL 口径 | 已验证 |

M4 验收命令：`cargo test --offline minimal::`（9 tests passed）、`git diff --check`、Node inline script syntax check passed。未引入自动拟合、图表刷选、设置中心或新接口。

## M5：删除旧框架、生命周期收口和真实历史验收 — 已验证

| 计划任务 | 实现/记录 | 测试或证据 | 状态 |
| --- | --- | --- | --- |
| 删除无消费者旧代码 | `src/minimal/`、`web/index.html`、`migrations/0001.sql` | `find src -type f` 仅保留最小入口；旧 Rust 分层、旧 Web 脚手架和旧迁移已删除；无旧模块编译引用 | 已验证 |
| 精简运行依赖 | `Cargo.toml`、`Cargo.lock` | 移除 `hmac`、`notify`、SQLx migrate/macros 特性；`cargo tree --offline` 仅保留最小运行依赖链 | 已验证 |
| 生命周期脚本切换 | `scripts/codex-meter-service.sh` | 临时脱敏 fixture 验证 `start/status/restart/stop`；健康路由为 `/api/health`；默认 DB 为 `.runtime/codex-meter-minimal.sqlite` | 已验证 |
| ccusage 独立校验 | `src/minimal/ccusage.rs`, `validation_runs` | ccusage 20.0.19 脱敏 fixture 实跑 `API/订阅 × daily/session × auto/standard` 8 次；日级和 Session Token 差值均为 0；报告无路径/认证字段 | 已验证 |
| 真实历史回填 | `/Users/Lendfating/.codex`（只读）→ `/tmp` 数据库 | 服务健康返回 `tables=7`；报告汇总 `18` 天、选中日 `80` 分钟、`3` 模型、`1` Session；8 个 ccusage 结果成功，72 条日 Token 对照中 32 条存在差值且原样保留供人工检查；报告未泄漏认证字段 | 已验证 |
| 保留原始回滚边界 | `.runtime/codex-meter.sqlite`、`.runtime/codex-meter.sqlite.m0-backup-20260805` | 旧数据库和备份未写入；真实烟测数据库均为 `/tmp` 临时文件 | 已验证 |

M5 验收命令：`cargo test --offline`（13 个最小实现测试通过）、`cargo build --offline`、`git diff --check`、Node inline script syntax check、临时服务 HTTP 200/健康/报告/ccusage 对账验收均通过。实现已收口；后续只修复核心目标所需的缺陷，不重新引入旧分层或新页面/服务。

## 本轮前端结构收口（对应 M2 页面一、M4 页面二展示门禁）— 已验证

| 计划任务 | 实现文件 | 测试/证据 | 页面或 API 验收 | 状态 |
| --- | --- | --- | --- | --- |
| 页面品牌与导航布局 | `web/index.html` | inline JS syntax check；`git diff --check` | 品牌移至每页顶栏，折叠侧栏只保留菜单按钮和图标导航 | 已验证 |
| 当前窗口五项汇总与显示口径 | `web/index.html` | Node 渲染脚本检查；`cargo test --offline`（13/13） | 账号 Token（截止昨日）、本机 Token、API 美元、本机 Credit/周额度、本机占比均有独立标签；缺失值保持待补数据 | 已验证 |
| 日期双源对比与 Session 可读性 | `web/index.html` | 临时本地服务 `/api/report` 返回 19 天/23 窗口；首页标记检查通过 | “JSONL 与 ccusage 对比”含推理 Token，不再显示质量列；Session 首列显示对话标题并保留 ID 次要信息 | 已验证 |
| 容量区间与合并对比图表 | `web/index.html` | 临时服务首页标记检查：`dual-slider`、`combined-chart`、`同区间对比明细` 均存在 | 双端点开始/结束滑块、官方百分比与本机 Credit 同图同表；缺失后端字段不以零或估算值填充 | 已验证 |

本轮验收命令：`git diff --check`、Node inline script syntax check、`cargo build --offline`、`cargo test --offline`（13 个最小实现测试通过）；临时服务健康接口返回 `schema=minimal-r1,tables=7`，首页关键前端标记检查全部通过。浏览器自动刷新被本机 Browser URL policy 拦截，因此未把受阻的浏览器截图作为验收证据。

## 本轮前端展示调整（用户确认，前端范围）— 已验证

| 计划任务 | 实现文件 | 测试/fixture | 页面或 API 验收 | 状态 |
| --- | --- | --- | --- | --- |
| 当前窗口显示当前套餐周额度 | web/index.html | Mock report 渲染检查 | 当前窗口新增“当前套餐周额度”独立卡片；未确认时显示“待确认”，不显示零值 | 已验证 |
| 日历三行指标 | web/index.html | Mock report 渲染检查；git diff --check | 每格显示 T 本机/账号、API 美元/Credit、Δ% 本机/官方；账号 Token 缺失显示 —；颜色使用本机 Credit/API 美元 | 已验证 |
| 日视图趋势指标 | web/index.html | Mock report 切到周窗口%并检查柱状图/Reset 标记 | 按钮顺序为 Token/API 美元/Credit/周窗口%；周窗口%包含当天本机/账号柱状图与 Reset 分段累计线；每日 Token/USD/Credit 不再标成跨天累计 | 已验证 |
| 分钟日内累计 | web/index.html | Mock selected_day minutes 渲染检查 | Token/API 美元/Credit 从当天 0 开始累计；官方百分比保留观测点和 Reset 分段说明；账号 Token 只作为日级参考 | 已验证 |
| 容量页前端展示 | web/index.html | Mock report 调用 capacityPage()；JS 语法检查 | 默认最近 30 天；同一 Reset 区间限制入口；双轴百分比/Credit 图；折叠明细；自动估算建议值入口；20/100/200 人工确认输入保留 | 已验证 |
| 每日套餐额度字段边界 | web/index.html | 仅使用 day.capacity_credit/day.plan_code 等显式字段；缺失字段渲染为空 | 前端不把当前套餐额度错误套到其他日期；缺字段显示待补，不修改后端采集或数据库 | 已验证 |
| Reset 窗口展示去除时间抖动 | web/index.html | Mock quota_windows 中秒级 reset_at 差异渲染检查 | 页面仅合并同账号/limit/window_kind 且 5 分钟内的显示抖动；未修改后端 Reset 算法或原始数据 | 已验证 |

前端展示调整阶段未修改 JSONL、App Server、ccusage、Rust report builder、数据库 schema 或采集逻辑。验收命令：node --check /tmp/codex-meter-inline-0.js、Mock report 页面函数渲染检查、git diff --check、cargo test --offline（13/13）。随后本轮单独新增了数据库 baseline，记录如下。

## 本轮新数据表 baseline（用户确认，数据库范围）— 已验证

| 计划任务 | 实现文件/数据库 | 测试与证据 | 页面/API 验收 | 状态 |
| --- | --- | --- | --- | --- |
| 新增最小数据模型 | `migrations/0002_minimal_data_model.sql`, `src/minimal/db.rs` | `cargo test --offline minimal::db::tests::minimal_schema_has_seven_new_tables` 通过；运行库 `PRAGMA integrity_check` 返回 `ok` | `source_jsonl`、`source_app_server`、`source_ccusage`、`usage_daily`、`usage_minute`、`usage_session`、`capacities_v2` 已创建 | 已验证 |
| 保留旧表 | `.runtime/codex-meter-minimal.sqlite` | 旧 `events`、`capacities` 等表结构哈希与备份一致；迁移只执行建表/索引 | 旧 API 暂继续使用旧表，后续采集迁移另行处理 | 已验证 |

本轮运行库备份：`.runtime/codex-meter-minimal.sqlite.pre-new-model-20260806`。备份在迁移前生成并通过完整性检查；运行服务在后台继续扫描，因此迁移前后旧事件计数可能自然增加，但旧表结构未改变。

## 第一批来源 Pipeline（用户确认，M1 范围）— 已验证

| 计划任务 | 实现文件 | 测试/fixture | 页面/API 验收 | 当前状态 |
| --- | --- | --- | --- | --- |
| JSONL → `source_jsonl` | `src/minimal/jsonl.rs`, `src/minimal/db.rs` | JSONL fixture 首次扫描写入 `session/usage/quota`；第二次扫描不重复；Turn 事件按稳定键更新；真实运行库快照已有 `session=22`、`turn=1487`、`usage=11932`、`quota=501` | 来源事实已落库，并作为第二批重建输入 | 已验证 |
| App Server → `source_app_server` | `src/minimal/app_server.rs`, `src/minimal/db.rs` | account/quota/usage fixture 写入 3 条精简快照；秘密字段测试通过 | 官方状态来源已具备，仍由现有 opt-in supervisor 调度 | 已验证 |
| ccusage → `source_ccusage` | `src/minimal/ccusage.rs`, `src/minimal/db.rs` | 归一化 daily/session 行字段单测；失败状态保留 | 独立校验事实已具备，仍不进入生产账本 | 已验证 |
| 采集频率与边界 | `docs/MINIMAL_SOURCE_PIPELINE.md`, `src/main.rs` | 10 秒 JSONL、60 秒 quota、6 小时 account/usage、启动/手工 ccusage 规则已固定 | 无新增后台服务；第二批由同一进程刷新入口触发 | 已验证 |

第一批只负责来源事实；第二批从三张来源表重建结果表，不能把来源表本身描述成页面结果。

## 第二批结果 Pipeline（本轮）— 已验证

| 计划任务 | 实现文件 | 测试/真实数据证据 | 当前状态 |
| --- | --- | --- | --- |
| 来源 → Daily/Minute/Session 结果 | `src/minimal/rollup.rs`, `src/minimal/mod.rs`, `src/main.rs`, `src/minimal/app_server.rs`, `src/minimal/server.rs` | `cargo test --offline`（18/18）；`cargo build --offline`；真实库 `PRAGMA integrity_check=ok`，`usage_daily=19`（2026-07-13 至 2026-08-06）、`usage_minute=3486`、`usage_session=995` | 已验证 |
| Reset/周窗口分段 | `src/minimal/rollup.rs` | 真实库存在 6 个 `window_id`；使用 `resets_at_ms` 变化（忽略 5 分钟内抖动）或明显百分比回落切段；无 `usage_weekly` 物理表 | 已验证 |
| 可重复重建与缺失口径 | `src/minimal/rollup.rs` | 在单事务内清空并重建三张结果表；未知计价、缺失 Turn 边界保留 `NULL`/quality，不填零 | 已验证 |

## 结果表 → API → 前端真实数据闭环（本轮）— 已验证

| 计划任务 | 实现文件 | 测试/真实数据证据 | 页面或 API 验收 | 当前状态 |
| --- | --- | --- | --- | --- |
| `/api/report` 切换到第二批结果表 | `src/minimal/report.rs` | `cargo test --offline`（19 tests passed）；`minimal-r1-rollup` fixture 断言日/分钟/窗口字段来自 `usage_*` | 真实报告返回 `days=19`、选中日 `2026-08-06`、结果表当前有 `486` 条分钟记录、`quota_windows=6`；页面 Session 按根对话聚合 | 已验证 |
| 页面调用真实报告数据 | `src/minimal/server.rs`, `web/index.html` | 本机回环服务 `/api/health` 返回 `status=ok, schema=minimal-r1, tables=14`；首页源码包含 `/api/report` | 浏览器 DOM 验收看到真实日期 Token、Reset 窗口、Session 和分钟趋势；窗口详情读取 `quota_windows[].minutes/sessions` | 已验证 |
| API 仍保持旧 fixture 可测试 | `src/minimal/report.rs` | 空结果表时保留旧 builder fallback；真实结果表存在时固定走 `minimal-r1-rollup` | 不改变四个既有路由契约，不添加新接口或新表 | 已验证 |
| 容量确认值写入新表 | `src/minimal/server.rs` | `/api/capacities` 仅接受有限非负 Credit，写入 `capacities_v2`；拒绝缺失/非法值 | 页面仍通过同一 POST 接口，报告从 `capacities_v2` 读取确认值 | 已验证 |
| 缺失计价/ccusage 口径 | `src/minimal/report.rs`, `web/index.html` | 真实 `source_ccusage=0`；未知模型/Fast 保持 `NULL`，没有伪造金额或校验结果 | 页面显示“待补数据/暂无验证结果”，Token 与官方百分比仍可正常展示 | 已验证 |

本轮真实库快照（2026-08-06，服务停止后的最终快照）：`source_jsonl=14090`、`source_app_server=3`、`source_ccusage=0`、`usage_daily=19`、`usage_minute=3512`、`usage_session=1133`、`capacities_v2=0`；日期范围 `2026-07-13` 至 `2026-08-06`，`window_id=6`。`PRAGMA integrity_check` 返回 `ok`。

本轮验收命令：`cargo test --offline`（19 passed）、`cargo build --offline`、`git diff --check`、`node --check` 内嵌脚本。服务启动后用回环 `curl` 验收 `/api/health` 和 `/api/report`；页面自动刷新在本机 Browser URL policy 下被拦截，已用浏览器 DOM 快照和首页源码检查作为页面证据。服务验收进程已停止，未提交或推送。

## 2026-08-07 七表运行链收口修订

上面较早的 M0–M5 条目保留为历史记录；其中关于 `minimal-r1`、14 张表、旧
`validation_runs`/fallback 和旧默认数据库路径的描述不再代表当前运行链。当前代码以
`docs/MINIMAL_CODE_INVENTORY.md` 和 `docs/MINIMAL_SOURCE_PIPELINE.md` 为准：

| 项目 | 当前结论 | 验收证据 |
| --- | --- | --- |
| 数据库初始化 | 只执行 `migrations/0002_minimal_data_model.sql`，新默认文件为 `.runtime/codex-meter-seven.sqlite` | 临时启动后 `sqlite_master` 仅有 `capacities_v2`、三张 `source_*`、三张 `usage_*` |
| 第一批 Pipeline | JSONL、App Server、ccusage 分别只写三张 `source_*` 表；JSONL 游标放在可重建 sidecar | `rg` 检查 Rust 无旧表 SQL；来源单测通过 |
| 第二批 Pipeline | `src/minimal/rollup.rs` 只重建 `usage_daily`、`usage_minute`、`usage_session` | `cargo test --offline`：17/17 通过；report 测试先执行 `refresh_rollups` |
| 报告/API | `report.rs` 只读七张正式表；不再回退旧表；health 返回 `schema=minimal-seven,tables=7` | 临时回环服务 `/api/health` 和 `/api/report` 均通过 |
| 历史运行库 | `.runtime/codex-meter-minimal.sqlite`、`.runtime/codex-meter.sqlite` 及备份未删除，仍作为旧数据归档候选 | 本轮未改动运行时数据库 |

本轮命令：`cargo fmt --all -- --check`、`cargo test --offline`（17 passed）、
`cargo build --offline`、`git diff --check`；临时数据库/服务验收使用
`CODEX_METER_DB=/tmp/codex-meter-seven.sqlite`，未触碰用户的 `~/.codex` 原始数据。
