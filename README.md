# ytdev-mcp

个人代码工作流 MCP server（stdio，单二进制）。把 `~/Plan/<project>/PLAN.md`、codex worker、review/门禁、发布前隐私与漏洞扫描固定成 7 个工具。

工具：`dev_projects` `dev_plan` `dev_plan_update` `dev_worker` `dev_review` `dev_gate` `dev_release`

自检：`./target/release/ytdev-mcp --selftest`（检查 gitleaks / osv-scanner / cargo / codex / git）

设计与决策：`~/Plan/yt-dev-mcp/PLAN.md`

```
cargo build --release
```
