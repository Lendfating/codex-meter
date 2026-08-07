# Codex Meter 实施状态

本文记录《最终执行计划》的实际落地状态，不替代主计划、最终设计或数据来源参考。

## 已落地的第一条纵向链路

- SQLite 迁移 `0003`—`0010`：canonical facts、JSONL/App Server 双源配额、账号 Token bucket、replay 标记、版本化定点价格、独立订阅/API pricing quality；缺失的本机 Token bucket 保持 NULL，不再用 0 伪装。
- JSONL：active/archived 扫描、游标/截断/半行/归档移动、白名单事实、last/cumulative 双证据、Session 关系、历史 rate-limit 回填和幂等重跑。
- App Server：NDJSON 行解析、稀疏 rate-limit/usage 通知落库、敏感字段过滤、账号 bucket 规范化、账号 HMAC/上下文切换，以及无 shell 的可选子进程 supervisor（初始化、轮询、退出重试和 stderr 脱敏）。
- ccusage：sanitized daily/session/raw 结果持久化、内部 rollup 与逐项差异保存、无 shell 的 `--offline` 命令执行入口；失败运行不会阻塞 JSONL 主账本。
- 计算：Token delta、fork replay、service tier、Reset window、分钟/Session/conversation/跨午夜 daily-session、Session 模型/tier 拆分、订阅 Credit/API USD 两套定点计价。
- API/页面：overview、days/day、minutes、sessions、quota、windows、calibrations、calibration-segments、capacity、methodology、reconciliation；页面一/二/三已经有可运行的本地壳和真实字段绑定。

## 已验证证据

- Rust 单元/集成测试：53 个库测试和 1 个主程序测试通过。
- `cargo clippy --all-targets --all-features --offline -- -D warnings` 通过。
- Web Node 测试：3 个测试通过。
- `git diff --check` 通过。
- 本机真实 `~/.codex` 历史只读烟测已建立 17 个本地日期；最近一次历史数据库约有 1.09 万条 JSONL 事实、9.3 千条 Token/配额观察、9.1 千条 usage delta 和 15 个 Reset window。未知模型会使对应日期/窗口金额显示为缺失并带 `missing_pricing`，不会被当成零金额；App Server 未运行时账号级 Token 参考保持 pending/NULL。
- 当前受限执行环境禁止监听 loopback socket，烟测日志最后会出现 `PermissionDenied`；该错误发生在 JSONL 采集和投影完成之后，不影响上述数据库结果。

## 仍未宣称完成的阶段

1. App Server supervisor 已能在显式环境变量开启时管理 `codex app-server --stdio`、初始化、账号/配额/usage 轮询、退出重试和安全日志；仍需真实登录环境验收请求/响应 multiplex、thread 元数据分页和七天重连指标，默认不开启。
2. ccusage 的命令执行入口已经存在，但尚未在本机默认自动运行；需要锁定实际二进制、参数、pricing override 和完整历史范围后执行发布门禁。
3. 容量页已能生成候选并写入 machine-scoped draft；confirmed 仍只允许显式写入，尚需补齐人工确认审计、区间选择和稳健拟合/量化误差。
4. 官方分钟百分比已经提供真实观察/carry-forward/stale 元数据，但尚未完成完整的 canonical timeline、同日多 Reset 分段和图表交互。
5. 设置/诊断、导出、备份、重算队列、真正 token 化的 CSRF、launchd 和七天连续运行验收尚未完成；当前写接口至少要求自定义确认头并拒绝非 loopback Origin。

因此当前交付物是“可运行、可追溯、可从真实历史回填的第一条纵向切片”，不是已经通过 R9—R11 全部门禁的最终生产版本。
