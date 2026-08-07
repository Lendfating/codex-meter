# 阶段 0：初始化与证据冻结

本阶段只建立可复现的工程边界和输入证据，不实现业务领域逻辑、SQLite、采集器、API 或业务页面。

## 资产

- Git 初始分支：`codex/initial-implementation`。
- Rust crate：根目录 `Cargo.toml` 与 `src/main.rs`。
- Web 空壳：`web/`，使用 Node 内置测试运行器，不引入业务页面。
- App Server schema：`fixtures/app-server/`。
- JSONL 脱敏 fixture：`fixtures/jsonl/`，由 `scripts/generate_sanitized_fixture.py` 生成。
- `ccusage` 版本和 `daily/session --json` 契约：`config/ccusage.lock.json` 与 `fixtures/ccusage/`。
- 历史 provider 人工映射：`fixtures/mappings/provider-history.json`。

当前冻结的本机证据版本：Codex `0.146.0-alpha.3.1`，App Server schema
347 个 JSON 文件；`ccusage` `20.0.19`，契约验证覆盖 `codex daily` 与
`codex session`；JSONL fixture 覆盖 `pro/null` 和 `openai/plus` 两个真实
provider/套餐样本。

## 隐私边界

fixture 只允许 `session_meta`、`token_count` 和 `thread_settings_applied` 的结构化白名单字段。生成器不复制提示词、回复正文、工作目录、邮箱、API Key、Authorization header 或完整第三方 URL；验证脚本会对所有字符串执行敏感信息门禁。

## 门禁

阶段 0 的完成条件是：fixture 隐私校验通过、schema/版本/映射资产存在且结构有效、`cargo test` 通过、`cd web && npm test` 通过。所有门禁通过后才进入阶段 1。
