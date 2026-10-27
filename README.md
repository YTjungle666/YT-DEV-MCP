# YT-DEV-MCP

个人代码工作流的 MCP server（stdio、单二进制、无异步运行时）。把 `~/Plan/<project>/PLAN.md`、codex worker 调度、独立评审/门禁、发布前隐私与依赖漏洞扫描固定成 8 个工具，让「计划 → 分工执行 → 收口 → 发版」这条链不用每次手搓命令。

## 工具

| 工具 | 作用 |
|---|---|
| `dev_projects` | 全局视图：每个项目的状态行、分支/脏文件/未推提交、活跃 worker |
| `dev_plan` | 把 PLAN.md 解析成结构化状态（状态行、决策表、阶段进度、YT 待办、下一步） |
| `dev_plan_update` | 定点写回计划：改状态行 / 勾一个步骤 / 追加 run 记录，返回 diff，匹配不唯一即拒写 |
| `dev_worker` | codex worker 生命周期：`start` / `start_batch` / `status` / `list` / `kill` |
| `dev_review` | 独立评审（强模型线），只看 diff，输出 JSON verdict + findings |
| `dev_gate` | 按仓库形态跑 fmt / clippy / test / build / make verify |
| `dev_release` | 发布预检：版本一致性、变更摘要、gitleaks 隐私扫描、osv-scanner 依赖漏洞、tag 命令预览 |
| `dev_publish` | 上传 GitHub：README/LICENSE 校验、密钥扫描门槛、`gh repo create`、main 分支保护 |

关键约束（写死在工具里，不是提示词）：

- 一个 worker 只拿计划的一个切片（`scope`，硬截断 4 KB），并行用 `start_batch`（上限 6），每个 worker 独立 `git worktree` + `yt-dev/<id>` 分支。
- CPU/内存有硬上限：`systemd-run --user --scope` 的 `MemoryMax` / `CPUQuota`，实测落在 cgroup 里；启动前磁盘余量不足 5 GB 直接拒绝。
- `kill` 只在显式调用时执行：先 SIGTERM 进程组，10 秒后 SIGKILL。
- 扫描命中一律脱敏输出（前 4 字符 + 长度），密钥原文不进报告。
- `dev_publish` 默认 dry-run，license / README / 密钥任一不过就不推送。

## 构建与安装

```sh
cargo build --release
./target/release/yt-dev-mcp --selftest   # 检查 gitleaks / osv-scanner / cargo / codex / git / gh
```

Hermes 侧注册（stdio）：

```sh
hermes mcp add yt-dev --command "$PWD/target/release/yt-dev-mcp"
```

手写 `~/.hermes/config.yaml` 时注意：stdio 子进程只继承过滤后的环境，`PATH` 必须包含 `gitleaks`、`osv-scanner`、`cargo` 所在目录。

```yaml
mcp_servers:
  yt-dev:
    command: /path/to/YT-DEV-MCP/target/release/yt-dev-mcp
    timeout: 900
    connect_timeout: 60
    env:
      PATH: ~/.local/bin:~/.cargo/bin:/usr/local/bin:/usr/bin:/bin  # 展开后的绝对路径
```

## 配置与状态

```
~/.config/yt-dev/
  gitleaks.toml      # 私有规则：家目录绝对路径 / 10.10.10.* / token 前缀 + allowlist
  projects.toml      # project → repo 路径显式映射（覆盖 PLAN.md 声明与同名目录启发式）
  worker-prompt.md   # worker prompt 模板（{project} {task} {scope} {slice} {plan_path} {mem_mb} {cpu_pct}）
~/Plan/<project>/runs/<id>.{json,log}   # run 元数据与 codex 原始输出
```

计划文档形状（`dev_plan` / `dev_worker` 依赖它）：

```
状态：vX（一句话结论）
| D1 | 已定结论 | 理由 |
## 硬约束 / ## 非目标 / ## 边界
### P0 骨架
- [ ] 一步能独立验收的活
## 下一步
```

## 依赖

外部二进制，缺了报错并给安装命令，不自动下载：`gitleaks`（隐私扫描）、`osv-scanner`（依赖漏洞）、`cargo`、`codex`、`git`，`dev_publish` 另需 `gh`。`dev_release` 的 `deep` 模式可选 `cargo-audit`。

## License

PolyForm Noncommercial License 1.0.0，见 `LICENSE`。派生自上游的部分沿用上游的 license。
