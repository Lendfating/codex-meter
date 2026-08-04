# 阶段 1：领域模型与 SQLite

阶段 1 建立单机存储基础，未进入 JSONL、App Server 或 ccusage 采集器实现。

## 已实现

- SQLite 版本迁移和连接池；连接启用 WAL、foreign keys 与 5 秒 busy timeout。
- 账号身份、上下文区间、JSONL 文件状态、token 观察、配额/账号用量快照、ccusage 快照。
- usage delta、日汇总、价格版本、容量版本、标定段、人工审计和采集运行记录。
- 所有时间字段以 UTC epoch 毫秒存储；提供 Asia/Shanghai 日期转换。
- 邮箱归一化后只保留 masked 值和本地 HMAC-SHA256 指纹。
- 派生结果携带 `source`、`quality`、`pricing_version`、`collector_version`。
- 原始观察/快照按机器与 source digest 去重；账号上下文区间由 SQLite trigger 禁止重叠，半开区间允许首尾相接。

## 阶段门禁

- 空库迁移、重复启动和迁移重复执行：通过。
- 相同原始 token 事件重复插入：第二次被忽略，计数保持为 1。
- 账号上下文区间：`[0,100)` 与 `[100,200)` 可共存，`[50,150)` 被数据库拒绝。
- Rust format、Clippy、全量测试：通过。
- 阶段 0 fixture/证据隐私校验与前端空壳测试：通过。

阶段 1 完成后停止；下一阶段的 JSONL 增量采集不在本阶段范围内。
