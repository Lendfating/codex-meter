# 阶段 2：JSONL 增量采集

阶段 2 建立本机 JSONL 事实账本，未进入 App Server 身份/配额采集。

## 已实现

- 递归扫描 `sessions` 与 `archived_sessions` 下的 JSONL 文件。
- 使用 `notify` 文件事件和 2 秒 `DebouncedPathQueue`；扫描游标持久化 inode、path、offset、mtime 和最后完整行摘要。
- 只解析 `session_meta`、`token_count`、`thread_settings_applied`，不保存其他事件或对话正文。
- 继承模型、provider 与 Standard/Fast 状态；token 事件使用 `last_token_usage` 作为本机增量，缺失时回退到累计值。
- 解析并保存白名单 rate-limit 投影、session metadata 和 thread settings，供后续账号归因使用。
- 通过 source digest、inode/path 游标和 SQLite 唯一约束处理重放、归档移动、active/archived 副本、截断与半行尾部。
- 按机器时区（默认 `Asia/Shanghai`）重建 `jsonl_daily_token_rollups`；该 Token 日汇总是参考数据，不参与 Credit 计算。

## 阶段门禁

- fixture 重放两次：原始 token、metadata、settings 和日汇总不重复。
- 同一 session 的 Standard/Fast/Standard：token 事件按顺序继承 `standard`、`fast`、`standard`。
- active 与 archived 相同文件：token 只入库一次。
- 半行尾部：未补全前不入库，补全后入库一次。
- 文件截断：游标重置后继续增量读取，既不丢失新事件，也不重复旧事件。
- 归档移动：同 inode 复用游标并保留单一文件状态。

阶段 2 完成后停止；App Server 子进程、账号身份和配额控制面留在阶段 3。
