# 精细化设计审查

## 审查结论

设计可以进入实现，但以下修订必须作为实现门禁。它们已经同步到最终设计和执行计划。

## 阻断级问题与处理

### P0-1 身份、套餐和容量档混在一起

问题：`model_provider`、`plan_type` 和 20/100/200 容量档不是同一个概念。尤其 provider 名为 `pro` 不代表 Pro 套餐，而 `plan_type=pro` 也不应自动决定 100/200 美元容量档。

处理：独立保存 `auth_kind`、`plan_type_raw`、`display_group`、`capacity_profile`。当前用户确认的 21 份 `model_provider=pro + plan_type=null` 用人工规则归入 `Other/API`；真正的 `plan_type=pro` 归入 ChatGPT Pro。

### P0-2 Credit 与美元共用一个计价结果

问题：订阅 Credit rate card 和 API 美元价格不是同一套价格，Fast 倍率也可能不同。把 `ccusage.costUSD` 同时当作 Credit 和美元会产生系统性误差。

处理：建立 `subscription_credit` 与 `api_usd_equivalent` 两套不可变价格方案；每套分别执行自动 Fast/强制 Standard 差分，所有结果带单位与 scheme。

### P0-3 稀疏配额通知可能破坏快照

问题：`account/rateLimits/updated` 是稀疏更新，缺失字段表示“本次没提供”，不是置空。

处理：通知只合并到最近完整快照；没有基线或出现冲突时执行 `account/rateLimits/read`，数据库同时保存原始通知和合并后快照来源。

### P0-4 账号切换附近的本机增量可能串账

问题：`ccusage` session JSON 是累计汇总；如果切换通知和检查点之间仍有 token，差分可能跨两个账号。

处理：账号变化前后立即检查点；结合 JSONL token 时间戳切分可切分部分；仍无法拆开的增量标记 `mixed_account` 并从容量标定样本排除。

### P0-5 localhost 写接口仍有 CSRF 风险

问题：恶意网页可以尝试请求 `127.0.0.1` 服务，仅绑定 loopback 不足以保护“保存容量、修改映射、重建数据”等写操作。

处理：严格 Origin、无通配 CORS、session/CSRF token；危险管理操作二次确认；服务不接受局域网监听配置。

## 高优先级问题与处理

### P1-1 Fast 汇总隐藏细节

`ccusage` 能在内部按事件识别 Fast，但 JSON 汇总不直接输出 Fast token。用同一价格方案的 auto 与 force-standard 差值获得附加量；无法识别 tier 的事件单独标记，不能静默假设。

### P1-2 高频扫描会随历史增长变慢

采用事件触发、debounce 和运行耗时自适应退避；完整对账降低频率，并记录版本、耗时与结果 hash。实现后必须用真实历史目录做性能基线。

### P1-3 account/usage/read 不能充当本机账本

该接口只作为账号侧 token 辅助校验。它没有模型、Fast 和 Credit 拆分，失败也不能阻塞核心计算。

### P1-4 最新联网价格会污染历史

生产计算固定 `--offline` 加版本化 override。联网只用于提示存在候选新价格，经人工确认生效时间后新增版本。

### P1-5 百分比整数化会制造虚假精度

容量标定至少选取 10 个百分点跨度，使用多点稳健估计，并显示量化误差；确认容量仍由人保存。

### P1-6 账号级每日 Token 延迟且日期时区未公开

问题：`account/usage/read` 的每日 bucket 是账号侧异步统计，当前日期可能缺失或延后补齐；schema 只给 `startDate`，没有公开时区字段。高频读取不能消除服务端延迟，还会制造重复样本。

处理：新增独立的 [Token 使用参考维度](TOKEN_USAGE_REFERENCE.md)。本机 JSONL 仍是本机 Token 主账本，账号 bucket 只用于已结算历史日期的对账。当前按本机配置时区对齐（当前 `Asia/Shanghai`），原始远程日期不改写，并保存 `pending/stale/settled` 新鲜度。两台机器的差值显示为“未观测 Token/误差”，不当作另一台机器精确用量，也不参与 Credit/窗口正式计算。

## 数据边界复核

- 账号总窗口：来自 App Server/JSONL rate limit，跨机器。
- 本机用量：来自本机 JSONL，经 `ccusage` 对账。
- 账号每日 Token：来自 App Server `account/usage/read`，可能跨机器且有延迟，只是 Token 参考维度。
- 其他机器：只能显示为账号总量减本机估算后的“未观测/误差”。
- 订阅容量：不存在官方累计 Credit 字段，只能人工确认。
- 历史官方 API/第三方 API：没有历史 base URL 时不能全部自动还原；允许人工映射，保留 unknown。

## 进入编码前门禁

1. 新任务的工作区根目录必须是 `/Users/Lendfating/git-code/codex-meter`。
2. 先完成 fixture 脱敏规则和字段白名单，才允许读取真实历史数据做测试。
3. 先锁定 `ccusage` 最低版本、JSON 输出契约和两套 pricing fixture，才允许实现金额计算。
4. P0 项必须各有自动测试；任何一项未覆盖，不能开始前端容量确认功能。
5. 第一版不承诺精确还原未知历史账号，也不实现多机同步。
6. Token 参考维度必须有本机时区、远程延迟、当前日期缺失和身份不可比较的自动测试。
