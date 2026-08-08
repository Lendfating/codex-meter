# Codex Meter

Codex Meter 是一个本地小工具，用来回答两个问题：

1. 当前账号在每天、分钟、模型和 Session 维度用了多少 Token，官方额度百分比如何变化；
2. 不同订阅的周窗口大约对应多少 Credit。

后端和页面由同一个 Rust 进程提供，不需要分别启动前后端。数据只保存在本机 SQLite：JSONL 是历史和本机明细主来源，App Server 是当前账号/官方配额补充，`ccusage` 只作为独立校验结果。未知模型或价格显示为“待定”，不会用 0 冒充已计算金额。

## 启动与验证

```sh
./service.sh start
./service.sh status
```

浏览器打开 <http://127.0.0.1:18778/>。停止、重启和查看日志：

```sh
./service.sh stop
./service.sh restart
./service.sh logs
```

脚本默认执行 `cargo build --offline`，默认数据库是
`.runtime/codex-meter.sqlite`，默认从 `~/.codex` 读取
`sessions/` 和 `archived_sessions/`。常用参数：

```sh
./service.sh start --port 18778
```

已有构建产物时可加 `--no-build`。App Server 始终启用（提供当前账号/
官方配额），其失败不影响 JSONL 主流程。

需要保存独立的 ccusage 对账时，显式打开它（每次会跑 API/订阅两套价格、
daily/session 和 `auto`/`standard`，共 8 个小结果）：

```sh
./service.sh restart --ccusage
```

打开后启动时立即对账一次，之后每 1 小时自动对账一次；
`POST /api/refresh` 也会顺带执行对账。ccusage 优先在系统 PATH 中查找，
找不到时通过 `npx` 临时安装官方 `ccusage@latest`，两者都没有则报错跳过。
结果会脱敏后写入 `source_ccusage`，页面一的"JSONL vs ccusage"
直接显示 Token/美元差值；ccusage 失败不会阻断 JSONL 主报告。

全部命令和参数（`--port`、`--no-build`、
`--ccusage`）见 `./service.sh --help`。

## 最小 API

- `GET /api/health`：服务和 8 张正式表是否正常；
- `GET /api/report?date=YYYY-MM-DD`：页面一、二、三所需的全部投影；
- `POST /api/refresh`：重新扫描 JSONL；
- `POST /api/capacities`：保存人工确认的 20/100/200 美元容量。

页面只有三块：用量（日历、趋势、分钟/模型/Session 和 JSONL/ccusage 对账）、容量估算（Reset 窗口候选和人工确认值）、计算说明（公式、价格版本和数据来源）。日、分钟、模型、Session、Reset 窗口都从 8 张事实表在内存中聚合，不再维护一套对应的派生表。

## 文档

- [最终设计](docs/FINAL_DESIGN.md)：完整目标和页面信息；
- [数据模型](docs/DATA_MODEL.md)：三页指标口径与八张表的最终结构；
- [来源 Pipeline](docs/SOURCE_PIPELINE.md)：JSONL、App Server、ccusage 的采集与物化；
- [数据来源参考](docs/DATA_SOURCE_REFERENCE.md)：JSONL、App Server、ccusage 的字段调研与 Token 参考维度。

项目不上传本地 JSONL、账号信息或 Token 数据，也不会自动修改订阅容量结论。
