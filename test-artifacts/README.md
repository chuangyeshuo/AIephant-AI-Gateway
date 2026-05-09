# 测试运行产物目录

本目录用于存放 **nextest / llvm-cov / 日志** 等机器可读归档，与设计文档
`docs/plans/2026-04-15-b-plus-c-test-harness-two-tier-design.md` 一致。

## 布局

- `runs/<run_id>/`：单次运行根目录（默认不提交 Git）。
  - `gate/`：门禁档 JUnit 等。
  - `nightly/`：夜间档 JUnit、覆盖率 `coverage/` 等。
  - `logs/`：终端整日志。
  - `manifest.json`：本次运行的元数据索引。

## `run_id` 约定

格式：`UTC时间戳-git短SHA`，例如 `20260415T120000Z-8e25285`。
可通过环境变量 `RUN_ID_OVERRIDE` 覆盖（供 CI 传入 `GITHUB_RUN_ID` 等）。

## 本地生成

见 `AGENTS.md` 中「测试归档」小节或执行：

`bash scripts/test-archival/run-gate.sh`
`bash scripts/test-archival/run-nightly.sh`（需本机 Postgres:54322 与 Redis，与集成测一致）
