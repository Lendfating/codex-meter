# Codex Meter 当前代码与数据清理清单

状态：2026-08-07，针对“七表 + 两批 Pipeline”进行的实际代码盘点。

本文只描述当前仓库的真实状态和清理边界，不新增产品功能。七张正式表仍以
`MINIMAL_DATA_MODEL.md` 为准。

## 1. 当前实际运行链

```text
第一批：JSONL ───────┐
       App Server ──┼─> source_* 三张来源表
       ccusage ─────┘

第二批：source_* 三张来源表
       └─> usage_daily / usage_minute / usage_session
           Reset/周窗口从 usage_minute.window_id 查询聚合

API：七张正式表 ─> src/minimal/report.rs ─> /api/report
前端：/api/report ─> web/index.html
```

当前活动代码入口只有 `src/main.rs` 和 `src/minimal/`。页面是单文件
`web/index.html`，服务脚本是 `scripts/codex-meter-service.sh`。

## 2. 七张正式表

| 表 | 粒度/职责 | 第一批/第二批 | 页面消费者 |
| --- | --- | --- | --- |
| `source_jsonl` | JSONL 的 session、turn、usage、quota 白名单事实 | 第一批 | 日、分钟、Session、官方历史回退 |
| `source_app_server` | 当前账号、套餐、官方 quota、账号日 Token 快照 | 第一批 | 当前窗口、官方状态、账号 Token |
| `source_ccusage` | ccusage daily/session 校验结果 | 第一批 | JSONL vs ccusage 对账 |
| `usage_daily` | 每个本地日期的 Token、金额、Credit、官方变化 | 第二批 | 日历、日趋势、账号 Token参考 |
| `usage_minute` | 分钟增量、官方采样、Reset `window_id` | 第二批 | 分钟图、周窗口、容量估算 |
| `usage_session` | 每个 Turn 的归一化用量，按 root 合并成 Session | 第二批 | Session 列表、模型汇总 |
| `capacities_v2`（逻辑名 `capacities`） | 人工确认的 20/100/200 美元档周 Credit | 人工确认 | 当前窗口、容量估算 |

不建立模型表、Reset 表、价格表、质量表或差异表。模型和窗口都从结果表查询聚合；
价格在静态代码配置中。

## 3. 两批 Pipeline 的代码落点

### 第一批：来源事实采集

| 来源 | 主要代码 | 写入 | 当前状态 |
| --- | --- | --- | --- |
| JSONL | `src/minimal/jsonl.rs` | `source_jsonl` | 全量/增量扫描、Token/Turn/Session/quota 解析；文件游标使用可重建的 JSON sidecar |
| App Server | `src/minimal/app_server.rs` | `source_app_server` | account/rateLimits/usage 三类调用，只写脱敏的来源快照 |
| ccusage | `src/minimal/ccusage.rs` | `source_ccusage` | daily/session 归一化和独立校验结果，只写来源快照，不参与本机账本 |

### 第二批：结果重建

| 输入 | 主要代码 | 输出 | 当前状态 |
| --- | --- | --- | --- |
| 三张 `source_*` 表 | `src/minimal/rollup.rs` | `usage_daily`、`usage_minute`、`usage_session` | 已能事务内重建；Reset 不另建表 |
| 结果表 | `src/minimal/report.rs` | `/api/report` 所需 JSON | 只读七张正式表；空结果保持 `NULL`/空集合，不回退旧表 |
| API | `src/minimal/server.rs` | health/report/refresh/capacities | 页面使用真实结果表；health 只统计七张正式表 |

## 4. 为什么当前数据库不是七张表

历史默认文件 `.runtime/codex-meter-minimal.sqlite` 是旧迁移和新迁移共同初始化的过渡库，
曾实际包含 14 张用户表。当前启动入口已经切换到新的
`.runtime/codex-meter-seven.sqlite`；新库只执行 `migrations/0002_minimal_data_model.sql`：

### 正式七张

`source_jsonl`、`source_app_server`、`source_ccusage`、`usage_daily`、
`usage_minute`、`usage_session`、`capacities_v2`。

### 旧七张

| 旧表 | 当前残留用途 | 处理结论 |
| --- | --- | --- |
| `files` | JSONL 文件游标和审计计数 | 已由 `.runtime/*.jsonl-cursors.json` sidecar 替代；旧库仅作归档 |
| `events` | 旧 JSONL 原始事件、旧 fallback、ccusage Session 映射 | 不再写入；旧库只作回滚归档 |
| `quota_samples` | 旧 quota 快照和旧 fallback | 不再写入；来源统一使用 `source_jsonl/source_app_server` |
| `account_snapshots` | 旧账号/usage 快照和旧 fallback | 不再写入；统一使用 `source_app_server` |
| `validation_runs` | 旧 ccusage stdout/对账快照 | 不再写入；统一使用 `source_ccusage` |
| `capacities` | 旧容量字段不兼容 | 不再写入；统一使用 `capacities_v2` |
| `settings` | 只保存 timezone/backfill 标记 | timezone 使用进程配置；backfill 由 cursor 是否存在判断 |

因此，之前对比时读取的数据库并非新的纯七表数据库，而是新旧 schema 的过渡库。
这是当前实现没有完全收口的事实，不是七表设计本身需要第八张表。

## 5. 清理顺序

### 本轮允许做（不删除用户数据）

1. 活动代码只初始化和读写七张正式表。
2. JSONL 文件游标迁移到可删除、可重建的 JSON sidecar。
3. 删除旧表写入、旧 report fallback、旧 validation 查询和旧 Session 路径查询。
4. 新鲜数据库的 health 只报告七张正式表（`schema=minimal-seven`）。
5. 启动脚本改用新的纯七表数据库路径；旧运行库保持原路径不动。

### 暂不做

- 不删除或覆盖 `.runtime/codex-meter-minimal.sqlite`、`.runtime/codex-meter.sqlite` 及备份；
- 不删除 `~/.codex` 原始 JSONL；
- 不批量删除旧文档和大型 App Server fixture；先等活动代码稳定后再按清单归档；
- 不改变页面信息架构、价格公式或三来源职责。

## 6. 验收门禁

清理完成后必须同时满足：

```text
新鲜 SQLite：除 sqlite 内部表外只有七张正式表
Rust：rg 不再发现旧表 SQL（允许测试中检查旧表不存在）
第一批：三来源只写三张 source_* 表
第二批：只重建三张 usage_* 表
API：/api/health 的 tables=7，/api/report 不依赖旧表
```

验收命令：

```bash
cargo test --offline
cargo build --offline
git diff --check
CODEX_METER_DB=/tmp/codex-meter-seven.sqlite \
CODEX_METER_DISABLE_COLLECTORS=1 \
target/debug/codex-meter
```

临时数据库通过 health 和 `sqlite_master` 检查后即可删除；真实运行库在用户确认前保留。

## 7. 当前剩余文件的归类

这些文件不属于线上两批 Pipeline，暂不混入活动代码：

| 路径 | 归类 | 当前处理 |
| --- | --- | --- |
| `migrations/0001.sql` | 旧七表迁移（`files/events/...`） | 已不被 Rust include 或启动入口引用；先保留作历史归档，后续确认后可移到 `docs/archive/legacy/` 或删除 |
| `docs/FINAL_EXECUTION_PLAN*.md`、`docs/PHASE_*.md`、旧审查文档 | 历史计划/研究 | 不参与编译和运行；保留，当前执行以 `MINIMAL_IMPLEMENTATION_PLAN.md` 为准 |
| `fixtures/`、`scripts/generate_*`、`scripts/validate_*`、`scripts/write_schema_manifest.py` | 脱敏测试夹具和隐私/契约门禁 | 测试与复现使用，不能作为生产数据源；保留 |
| `pricing/*` | 空目录占位 | 实际价格配置在 `src/minimal/pricing.rs`；不创建价格数据库表 |
| `.runtime/*` | 本机运行库、WAL、日志、PID、cursor sidecar | 不纳入 Git；旧混合库和旧大库不删除，新的服务默认使用 `codex-meter-seven.sqlite` |
| `target/` | Rust 构建产物 | 可由 `cargo build` 重建，不作为源代码清理对象 |

因此本轮“清代码”已经完成运行链收口，但没有把历史文档、脱敏夹具或本机旧数据库
误删。若要进一步缩减仓库，只需单独确认 `migrations/0001.sql` 和历史文档归档范围，
不需要再改 Pipeline 或数据库设计。
