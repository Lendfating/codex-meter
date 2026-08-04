# 最终执行计划

## 1. 执行目标

在 `/Users/Lendfating/git-code/codex-meter` 实现一个单机后台服务和本地网页，持续采集当前机器的 Codex 用量，并同时展示：

- 当前账号总周窗口百分比与 reset。
- 当前机器按 Credit 估算的周窗口占比。
- 每天/每个窗口的 token、订阅 Credit、Fast 附加 Credit和 API 等价美元。
- Plus/Pro/Other(API) 历史归类与质量标签。
- 20/100/200 美元容量候选值和人工确认值。

项目不修改 `ccusage` 源码、不做多机通信、不自动认定订阅容量。

## 2. 已冻结的业务规则

编码前不再重新讨论以下规则：

1. `plan_type_raw=plus` 显示为 Plus。
2. `plan_type_raw=pro` 显示为 Pro。
3. 当前历史中 `model_provider=pro + plan_type_raw=null` 的 21 份记录，按用户确认的本地映射显示为 `Other/API`。
4. 其他没有证据的历史记录保留 Unknown，不套用第 3 条通用推断。
5. `auth_kind`、`plan_type_raw`、`display_group`、`capacity_profile` 分开存。
6. 20/100/200 容量档只由人选择和确认；`plan_type=pro` 不自动判断 100 或 200 美元档。
7. App Server/JSONL 的窗口百分比是账号总量；本机量来自本机 JSONL 和 `ccusage`。
8. 订阅 Credit与 API 美元是两套 pricing scheme，两者 Fast 倍率独立。
9. 价格变更边界为 `2026-08-01T07:00:00Z`，即北京时间 `2026-08-01 15:00:00`。
10. 最新价格不能自动覆盖历史；所有生产计算使用离线、版本化价格。

## 3. 技术方案与目录

第一版采用单个 Rust 后端 crate，避免过早拆成多个内部 crate；前端独立构建后嵌入二进制。

```text
codex-meter/
  Cargo.toml
  src/
    main.rs
    config.rs
    domain/
    storage/
    collectors/
      jsonl.rs
      app_server.rs
      ccusage.rs
    attribution/
    pricing/
    calibration/
    api/
    scheduler.rs
  migrations/
  pricing/
    subscription-credit/
    api-usd/
  web/
    src/
      pages/
      components/
      charts/
      api/
  fixtures/
    jsonl/
    app-server/
    ccusage/
  docs/
  scripts/
  launchd/
```

建议依赖：

- Rust：Tokio、Axum、SQLx/SQLite、Serde、Notify、Tracing、Time。
- Web：React、TypeScript、Vite、TanStack Query、ECharts。
- 测试：Rust 单元/集成测试、Vitest、Playwright。

服务只监听 `127.0.0.1`，前端与 API 同源；写接口使用 Origin 与 CSRF 校验，不开启通配 CORS。

## 4. 分阶段实施

### 阶段 0：初始化与证据冻结

任务：

- 在目标目录初始化 Git，首个分支使用 `codex/initial-implementation`。
- 创建 Rust、Web 和文档骨架，不先实现业务页面。
- 保存 Codex `0.146.0-alpha.3.1` App Server schema 快照。
- 从真实 JSONL 生成只保留结构化白名单字段的脱敏 fixture。
- 固定当前 `ccusage` 最低兼容版本及 `daily/session --json` 输出契约。
- 写入本地历史映射：`provider=pro + plan=null -> Other/API`，来源标记 `manual`。

门禁：

- fixture 中不得出现提示词、回复正文、邮箱明文、API Key、Authorization header 或完整第三方 URL。
- `cargo test` 与前端空壳测试通过后才进入阶段 1。

### 阶段 1：领域模型与 SQLite

任务：

- 建立 migration 和数据库连接池，启用 WAL、foreign keys、busy timeout。
- 实现账号身份、上下文区间、token 观察、quota 快照、ccusage 快照、usage delta、价格版本、容量版本、标定段和审计表。
- 时间统一存 UTC epoch 毫秒，展示时转换 Asia/Shanghai。
- 实现本地 HMAC 身份指纹；邮箱仅保存 masked 值和 HMAC。
- 为每条派生结果保存 `source`、`quality`、`pricing_version`、`collector_version`。

门禁：

- migration 可从空库建立，也可重复启动。
- 同一原始事件重复插入不翻倍。
- 账号区间有数据库约束或事务检查，不能产生重叠有效区间。

### 阶段 2：JSONL 增量采集

任务：

- 扫描 `~/.codex/sessions` 与 `~/.codex/archived_sessions`。
- 使用文件事件加 2 秒 debounce，按 inode/path/offset 增量读取。
- 白名单提取 `session_meta`、`token_count`、`thread_settings_applied`。
- 识别 model、provider、token、Fast、rate limit 和 session/turn 标识。
- 处理半行写入、文件截断、归档移动、active/archived 重复。
- 每 6 小时完整一致性扫描，不保存消息正文。

门禁：

- fixture 重放两次结果相同。
- 同一会话 Standard/Fast/Standard 切换能按事件继承。
- 文件在 active 与 archived 同时存在时只计一次。
- 尾部半行在补全前不入库，补全后只入一次。

### 阶段 3：App Server 身份与配额

任务：

- 管理 `codex app-server` 子进程、初始化、通知、心跳和自动重连。
- 调用 `account/read`、`account/rateLimits/read`、`account/usage/read`。
- 处理 `account/updated`、`account/rateLimits/updated`、`thread/tokenUsage/updated`。
- 对稀疏配额通知执行 merge；没有完整基线时重新 read。
- 活跃时 60 秒、空闲时 5 分钟兜底读取；reset 前后额外采样。
- 账号/auth/provider 变化时关闭旧上下文并开启新区间。

门禁：

- ChatGPT Plus、ChatGPT Pro、API Key 和不可识别 provider fixture 分类正确。
- 缺失字段不会把上一快照的有效值清空。
- App Server 断线不影响 JSONL 与 ccusage；恢复后自动补完整快照。
- `account/usage/read` 失败只降低辅助数据新鲜度。

### 阶段 4：ccusage 黑盒桥接

任务：

- 发现可执行文件，检查版本与 Codex report 能力。
- 所有生产调用固定 `--offline`，使用项目生成的 pricing override。
- 对 `subscription_credit` 执行 auto 与 force-standard。
- 对 `api_usd_equivalent` 执行独立 auto 与 force-standard。
- 保存 session 模型 token、累计值、scheme、版本、命令耗时与结果 hash。
- JSONL 变化后 10 秒 debounce；最小间隔为 `max(60 秒, 上次耗时 × 10)`。
- 账号切换/reset 立即检查点；每天与每 6 小时完整对账。

门禁：

- 同一 fixture 与直接运行同版本 `ccusage` 完全一致。
- `auto - standard` 只形成对应 scheme 的 Fast 附加量。
- Credit 与美元单位不能在类型/API/数据库字段中互换。
- 命令失败不覆盖上一份有效快照，stderr 只保存脱敏摘要。

### 阶段 5：账号归因和质量规则

任务：

- 对相邻 session 快照做差，生成最小 usage delta。
- 用 App Server 上下文和 JSONL 事件时间归因。
- 应用历史人工 provider 映射，但保留 raw provider/plan。
- 实现 `exact`、`estimated`、`mixed_account`、`unknown_provider`、`fast_unknown`、`missing_samples`。
- 跨账号且不能拆分的 delta 不进入容量标定。

门禁：

- `plan_type=pro` 与 `provider=pro` 两类 fixture 不会互相误判。
- 账号切换前后 token 能拆则拆，不能拆则明确降级。
- 任何人工映射都可追溯、可撤销，撤销后能重新汇总。

### 阶段 6：价格版本与历史回填

任务：

- 建立订阅 Credit/API 美元两套不可变价格目录和 schema 校验。
- 写入旧价、新价、模型别名、长上下文和 Fast 倍率 fixture。
- 价格按事件时间选择；联网查询只生成候选版本。
- 实现北京时间 2026-08-01 15:00 的 JSONL 前缀视图差分。
- 临时目录使用 0700，所有退出路径清理。
- 对无法精确切分的历史段标记 `boundary_approximate`。

门禁：

- 边界前 1 毫秒和边界后选择不同版本。
- 旧段 + 新段 token 等于完整 token，误差为 0。
- 两套 scheme 的 Fast 倍率差异有独立测试。
- 重新添加最新价格不会改变旧记录使用的 version ID。

### 阶段 7：窗口与容量标定引擎

任务：

- 用 account + limit ID + window kind + resetsAt 建立窗口 ID。
- 识别 usedPercent 下降、resetsAt 变化和 reset 附近缺样。
- 计算本机日/周 Credit 占比、账号剩余、账号已用和未观测/误差。
- 实现至少 10 个百分点跨度的多点稳健容量候选估算。
- 管理 usd20/usd100/usd200 的 draft/confirmed/version。
- Plus/Pro/Other/API 按当时上下文选择展示；API 不计算订阅窗口占比。

门禁：

- 未 confirmed 的容量不进入主页正式百分比。
- reset 后本机累计线归零，不跨窗口连线。
- Pro 共享账号的账号下降不会全部归给本机。
- 百分比整数取整误差反映在候选容量区间中。

### 阶段 8：本地 API 与安全

任务：

- 实现 overview、calendar、day detail、window series、calibration、settings、health API。
- 区分只读和写接口；写接口要求 session/CSRF token 与合法 Origin。
- 输入校验、分页、导出和统一错误格式。
- API 返回 raw、display、quality、freshness，前端不自行猜分类。

门禁：

- 非法 Origin、无 CSRF token 的写请求被拒绝。
- 无 CORS 通配、无局域网监听。
- SQL/路径输入有边界测试；API 不返回邮箱明文或 endpoint 全文。

### 阶段 9：前端

任务：

- 用量主页：顶部账号/窗口摘要、左侧日历、右侧三线窗口图、日详情。
- 三线图：账号剩余、账号已用、本机累计；共享账号显示未观测/误差区域。
- 容量标定页：20/100/200 confirmed 值、区间刷选、候选容量和污染提示。
- 设置/诊断：provider 映射、账号上下文、采集健康、重建与导出。
- 显示 Plus/Pro/Other(API)/Unknown，详情可展开 raw provider/plan 与归类来源。

门禁：

- Playwright 覆盖 Plus 单机、Pro 共享、API、跨账号日、reset、缺样和空状态。
- 1440×900、1920×1080 及窄窗口无重叠和横向溢出。
- 套餐情景切换只改变假设占比，不修改历史 context/capacity。

### 阶段 10：后台运行和验收

任务：

- 提供 launchd plist 生成、install/status/restart/uninstall。
- 数据库备份、日志轮转、JSON/CSV 导出、重新对账。
- 连续运行 7 天，记录 CPU、内存、ccusage 扫描耗时、断线恢复和数据差异。
- 完成用户手工验收：Plus 单机标定、Pro 共享差值、一次账号切换、一次 Fast 切换。

门禁：

- 重启电脑后服务自动恢复。
- 空闲 CPU 接近 0，数据库和日志增长可控。
- 七天内无重复计数；`ccusage` 对账差异为 0，或每个差异都有质量原因。
- 卸载默认保留数据库并明确提示位置。

## 5. 每阶段标准验证命令

项目建立后统一提供以下入口，避免开发人员记忆零散命令：

```bash
just format
just lint
just test
just test-integration
just web-test
just e2e
just check
```

`just check` 必须包含 Rust format/clippy/test、前端 typecheck/lint/test 和 pricing/fixture schema 校验。依赖安装只在首次初始化阶段进行。

## 6. 建议提交顺序

每个阶段至少一个可独立回退的 Conventional Commit：

```text
chore: scaffold codex meter
feat(storage): add versioned usage schema
feat(collector): ingest codex jsonl incrementally
feat(collector): track app server account quotas
feat(ccusage): reconcile credit and usd schemes
feat(attribution): classify account usage intervals
feat(pricing): version historical codex rates
feat(calibration): estimate and confirm plan capacity
feat(api): expose local usage endpoints
feat(web): build usage and calibration dashboards
feat(service): add launchd lifecycle
```

不把多个阶段压成一个大提交；阶段门禁未通过不进入下一阶段。

## 7. 第一版完成定义

以下条件必须全部满足：

1. 后台服务连续运行 7 天，无重复计数、持续高 CPU 或失控增长。
2. 本机 token 与固定版本 `ccusage` 一致。
3. 订阅 Credit、API 美元和各自 Fast 附加量有独立测试。
4. Plus、Pro、当前人工确认 API 和 Unknown 分类不会互相覆盖。
5. 主页明确区分账号总配额和本机估算。
6. 20/100/200 容量只有人工确认后才生效。
7. 历史价格按版本和时间计算，边界有精确或显式近似状态。
8. App Server 断线、稀疏更新和 usage 失败都不会破坏核心账本。
9. 服务只允许本机访问，数据库不含对话正文和认证秘密。
10. 用量主页和容量标定页通过真实样本人工验收。

## 8. 实施方式建议

正式编码应在 Codex App 中新建一个以 `/Users/Lendfating/git-code/codex-meter` 为工作区根目录的任务。当前任务工作区绑定的是 `ccusage`，继续在这里实现会导致兄弟目录写权限、检索范围和 Git 操作都不自然，也更容易误改 `ccusage`。

新任务的首条指令建议为：

```text
请先完整阅读 README.md、docs/RESEARCH_FINDINGS.md、docs/FINAL_DESIGN.md、
docs/DESIGN_REVIEW.md 和 docs/IMPLEMENTATION_PLAN.md。严格按最终执行计划实施，
先完成阶段 0，并在门禁全部通过后汇报，不要提前进入阶段 1。
```

本任务保留为调研与设计记录；新任务负责实现。文档已经包含必要上下文，不依赖复制整段聊天记录。
