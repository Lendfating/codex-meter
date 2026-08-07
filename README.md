# Codex Meter

Codex Meter 是一个本地小工具，用来回答两个问题：

1. 当前账号在每天、分钟、模型和 Session 维度用了多少 Token，官方额度百分比如何变化；
2. 不同订阅的周窗口大约对应多少 Credit。

后端和页面由同一个 Rust 进程提供，不需要分别启动前后端。数据只保存在本机 SQLite：JSONL 是历史和本机明细主来源，App Server 是当前账号/官方配额补充，`ccusage` 只作为独立校验结果。未知模型或价格显示为“待定”，不会用 0 冒充已计算金额。

## 启动与验证

```sh
./scripts/codex-meter-service.sh start
./scripts/codex-meter-service.sh status
```

浏览器打开 <http://127.0.0.1:18778/>。停止、重启和查看日志：

```sh
./scripts/codex-meter-service.sh stop
./scripts/codex-meter-service.sh restart
./scripts/codex-meter-service.sh logs
```

脚本默认执行 `cargo build --offline`，默认数据库是
`.runtime/codex-meter-seven.sqlite`，默认从 `~/.codex` 读取
`sessions/` 和 `archived_sessions/`。常用覆盖项：

```sh
CODEX_METER_DB=/tmp/codex-meter.sqlite \
CODEX_HOME="$HOME/.codex" \
CODEX_METER_PORT=18778 \
./scripts/codex-meter-service.sh start
```

已有构建产物时可设置 `CODEX_METER_SKIP_BUILD=1`。App Server 只在明确设置
`CODEX_METER_APP_SERVER_ON_BOOT=1` 时启用；不启用也可以完整查看 JSONL 历史。

需要保存独立的 ccusage 对账时，明确打开它（每次会跑 API/订阅两套价格、
daily/session 和 `auto`/`standard`，共 8 个小结果）：

```sh
CODEX_METER_CCUSAGE_ON_BOOT=1 \
CODEX_METER_CCUSAGE_BIN=/path/to/ccusage \
./scripts/codex-meter-service.sh restart
```

也可以设置 `CODEX_METER_CCUSAGE_ON_REFRESH=1`，让 `POST /api/refresh` 同时
执行对账。结果会脱敏后写入 `source_ccusage`，页面一的“JSONL vs ccusage”
直接显示 Token/美元差值；ccusage 失败不会阻断 JSONL 主报告。

## 最小 API

- `GET /api/health`：服务和 7 张正式表是否正常；
- `GET /api/report?date=YYYY-MM-DD`：页面一、二、三所需的全部投影；
- `POST /api/refresh`：重新扫描 JSONL；
- `POST /api/capacities`：保存人工确认的 20/100/200 美元容量。

页面只有三块：用量（日历、趋势、分钟/模型/Session 和 JSONL/ccusage 对账）、容量估算（Reset 窗口候选和人工确认值）、计算说明（公式、价格版本和数据来源）。日、分钟、模型、Session、Reset 窗口都从 7 张事实表在内存中聚合，不再维护一套对应的派生表。

## 文档

- [最小执行计划](docs/MINIMAL_IMPLEMENTATION_PLAN.md)：当前唯一执行边界和阶段门禁；
- [最终设计](docs/FINAL_DESIGN.md)：完整目标和页面信息；
- [数据来源参考](docs/DATA_SOURCE_REFERENCE.md)：JSONL、App Server、ccusage 的字段调研；
- [执行状态](docs/MINIMAL_IMPLEMENTATION_STATUS.md)：每个阶段的测试和验收证据。

项目不上传本地 JSONL、账号信息或 Token 数据，也不会自动修改订阅容量结论。
