# Codex Meter

Codex Meter 是一个完全本地运行的 Codex 用量与订阅窗口估算工具。每台机器独立采集、独立存储，不上传数据，也不与其他机器同步。

当前仓库按执行计划从阶段 0 开始实现。阶段 0 只包含可复现的初始化骨架、协议证据快照和脱敏 fixture；业务领域模型、采集器、数据库和页面仍未开始实现。

## 文档

- [调研结论](docs/RESEARCH_FINDINGS.md)：Codex JSONL、App Server、账号/API 识别、Credit 与配额字段的边界。
- [最终设计](docs/FINAL_DESIGN.md)：系统架构、采集频率、数据库、计算规则、价格版本和前端页面。
- [精细化设计审查](docs/DESIGN_REVIEW.md)：阻断级风险、修订结论和剩余边界。
- [执行计划](docs/IMPLEMENTATION_PLAN.md)：按阶段实施、测试和验收标准。

## 已固定的核心决策

1. JSONL 是本机 token、模型、Fast 状态和历史配额样本的主事件源。
2. Codex App Server 保留，用于实时账号身份、登录方式、套餐、账号总配额和切换事件。
3. `ccusage` 作为不修改源码的黑盒计算引擎，通过 JSON 输出提供去重后的 token 与成本；系统不 fork 它。
4. 订阅周窗口容量不是官方可读取字段，20/100/200 美元套餐的容量由人工确认后保存。
5. App Server/JSONL 中的窗口百分比是账号总量；`ccusage` 计算的是本机日志量，两者不可直接混为一条指标。
6. 2026 年 7 月价格变更按人工确定的 `2026-08-01T00:00:00-07:00` 生效，即北京时间 `2026-08-01 15:00:00`。
7. 认证类型、服务端 `plan_type`、本地容量档位是三个独立维度；UI 可把已确认的 API/provider 汇总为 `Other/API`，数据库保留原始值。
8. 订阅 Credit 与 API 等价美元使用两套独立价格方案和 Fast 倍率，不能复用一个 `costUSD` 数字。

## 阶段 0 资产

- `fixtures/app-server/`：由本机 Codex CLI 生成的 App Server schema 快照。
- `fixtures/jsonl/`：仅保留结构化白名单字段的脱敏 JSONL fixture。
- `fixtures/ccusage/`：固定的 `ccusage` 版本与 `codex daily/session --json` 输出契约。
- `fixtures/mappings/`：当前本机历史 `model_provider=pro` 且 `plan_type=null` 到 `Other/API` 的人工映射。
- `scripts/`：fixture 生成与隐私门禁脚本。
