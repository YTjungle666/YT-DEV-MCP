# YT-DEV-MCP

个人代码工作流的 MCP server（stdio、单二进制、无异步运行时）。把 `~/Plan/<project>/PLAN.md`、codex worker 调度、独立评审/门禁、发布前隐私与依赖漏洞扫描固定成 8 个工具，让「计划 → 分工执行 → 收口 → 发版」这条链不用每次手搓命令。

## 工具

| 工具 | 作用 |
|---|---|
| `dev_projects` | 全局视图：每个项目的状态行、分支/脏文件/未推提交、活跃 worker |
| `dev_plan` | 把 PLAN.md 解析成结构化状态（状态行、决策表、阶段进度、待办、下一步） |
| `dev_plan_update` | 定点写回计划：改状态行 / 勾一个步骤 / 追加 run 记录，返回 diff，匹配不唯一即拒写 |
| `dev_worker` | codex worker 生命周期：`start` / `start_batch` / `status` / `list` / `kill` |
| `dev_review` | 独立评审（强模型线），只看 diff，输出 JSON verdict + findings |
| `dev_gate` | 按仓库形态跑 fmt / clippy / test / build / make verify |
| `dev_release` | 发布预检：版本一致性、变更摘要、gitleaks 隐私扫描、osv-scanner 依赖漏洞、tag 命令预览 |
| `dev_publish` | 上传 GitHub：README/LICENSE 校验、密钥扫描门槛、`gh repo create`、main 分支保护 |

关键约束（由 `~/.config/yt-dev/config.toml` 决定，缺键取内置默认，不是写在提示词里）：

- 一个 worker 只拿计划的一个切片（`scope`，按 `[limits] slice_max_chars` 硬截断，默认 4000 字符），并行用 `start_batch`（上限 `[limits] max_parallel`，默认 6），每个 worker 独立 `git worktree` + `<branch_prefix><id>` 分支。
- CPU/内存有硬上限：`systemd-run --user --scope` 的 `MemoryMax` / `CPUQuota`（`[limits] mem_mb` / `cpu_pct`，可按次覆盖），实测落在 cgroup 里。
- 磁盘有产物上限（不是看剩余空间）：run 日志/元数据 + worktree 的总占用受 `[limits] max_store_mb` 约束（默认 4096 MB，可按次覆盖），触顶时先按 `keep_runs` 保留最近若干 run，再删最旧的已结束 run，最后删其遗留 worktree；运行中的 worker 永不删。
- `kill` 只在显式调用时执行：先 SIGTERM 进程组，等 `[limits] kill_grace_secs`（默认 10s）再 SIGKILL。
- 扫描命中一律脱敏输出（前 4 字符 + 长度），密钥原文不进报告。
- `dev_publish` 默认 dry-run，license / README / 密钥任一不过就不推送；强推与删除永久禁用（这两项不开放配置）。


## 构建与安装

```sh
cargo build --release
./target/release/yt-dev-mcp --selftest     # 检查配置解析 + 必需二进制
./target/release/yt-dev-mcp --init-config  # 生成带注释的 config.toml（已存在则不覆盖）
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

## 配置

只有一个可调文件：`$YT_DEV_CONFIG_DIR/config.toml`（默认 `~/.config/yt-dev/config.toml`）。全部键可省略，省略就是内置默认；路径里的 `~/` 会展开；未知键忽略；改了下一次工具调用就生效，不用重启 MCP；解析失败会在 stderr 留一行并退回默认值。

```sh
./target/release/yt-dev-mcp --init-config   # 写出带注释的完整例子（已存在不覆盖）
```

| 段 | 管什么 |
|---|---|
| `[paths]` | 计划根目录、产物目录、projects.toml、worker/review prompt、gitleaks 规则、license 正文、CI 模板 |
| `[limits]` | 产物上限、并发、各项截断、超时、kill 宽限 |
| `[plan]` | PLAN.md 词汇表：文件名、阶段/验证/待办/下一步/约束段落标题、状态行前缀、决策编号前缀、仓库声明关键字 |
| `[worker]` | codex 可执行文件与参数、nice、分支前缀、worktree 目录、硬约束条目 `rules`、模板里的行数上限 |
| `[review]` | 评审可执行文件与参数（`{out}` 占位）、prompt 模板 |
| `[publish]` | 默认 license、主干分支名、CI 文件路径、remote 模板、required checks、admin 直推、可见性 |
| `[gate]` | 门禁 profile（`detect` 文件 + `steps`）与按项目覆盖 |
| `[selftest]` | 必需二进制与安装提示 |

加一种语言的门禁、或某个仓库的私有检查，只加配置不改代码：

```toml
[[gate.profiles]]
name = "go"
detect = ["go.mod"]
steps = [
  { cmd = ["go", "vet", "./..."] },
  { name = "test", cmd = ["go", "test", "./..."], full_only = true },
]

[gate.overrides]                       # 单项目覆盖所有 profile
demo = [{ name = "lint", cmd = ["make", "lint"] }]
```

`steps` 三个可选项：`name`（报告里的步骤名，省略用 cmd 第一个词）、`full_only`（只在 `scope=full` 跑）、`needs`（命中文件内容里必须出现的字样，比如 package.json 有没有 `"test"`）。

状态与可选覆盖文件：

```
~/.config/yt-dev/
  config.toml        # 唯一可调文件
  projects.toml      # project → repo 路径显式映射（覆盖 PLAN.md 声明与同名目录启发式）
  gitleaks.toml      # 私有规则：家目录绝对路径 / 本地内网地址 / token 前缀 + allowlist
  worker-prompt.md   # 可选：覆盖 [worker] prompt，占位符 {project} {task} {scope} {slice} {plan_path} {rules} {mem_mb} {cpu_pct} {max_file_lines} {max_fn_lines}
  review-prompt.md   # 可选：覆盖 [review] prompt，占位符 {base} {task}
  LICENSE            # 可选：dev_publish 写进别的仓库的 license 正文
  ci.yml             # 可选：dev_publish 写进别的仓库的 CI 模板
~/Plan/<project>/runs/<id>.{json,log}   # run 元数据与 codex 原始输出
```

计划文档形状（`dev_plan` / `dev_worker` 依赖它，标题词可在 `[plan]` 改）：


```
状态：vX（一句话结论）
| D1 | 已定结论 | 理由 |
## 硬约束 / ## 非目标 / ## 边界
### P0 骨架
- [ ] 一步能独立验收的活
## 下一步
```

## 语言与编译约定

这些是 worker prompt 里 `[worker] rules` 数组的条目（默认 10 条），不是代码：

- 新项目默认 Rust；已有源码的仓库沿用原语言，除非计划里明确写了重构/迁移。换语言不由 worker 决定，写进收尾说明交 YT。
- Rust 日常验证用 `cargo check` / debug `cargo build`，`--release` 收尾验一次；release 用 `opt-level = 3` + `lto = "thin"`，fat LTO 只留给 CI 发版。

同一份默认 rules 里还有两条通用硬约束：不许硬编码（路径/端口/上限/命令/URL 都从配置读并给默认值）、可扩展（多一种实现或多一个可调项只加配置或新模块，不改既有调用方）。改措辞、删一条、加一条只改 `[worker] rules`；改整体版式才动 `worker-prompt.md`。`dev_review` 的评审 prompt 同样会盯这两条。

`dev_gate` 的 rust profile 会读仓库的 `Cargo.toml`，发现 fat LTO 或 `opt-level = "z"/"s"` 时在报告顶部打 `hint:`（本仓实测：fat LTO 每次改行重链 7.8s，关掉 1.2s）。


本机全局加速（不是本仓必需）：`sccache` 作 `rustc-wrapper`、`mold` 作链接器，写进 `~/.cargo/config.toml`。坑：sccache 守护进程在 `$TMPDIR` 下建临时文件，而 agent 会话的 TMPDIR 每轮会变，直接用 `sccache` 会出现 `Failed to create temp dir` 让编译失败；用固定 TMPDIR 的包装脚本（`~/.local/bin/sccache-wrap`）。

## 依赖

外部二进制，缺了报错并给安装命令，不自动下载：`gitleaks`（隐私扫描）、`osv-scanner`（依赖漏洞）、`cargo`、`codex`、`git`，`dev_publish` 另需 `gh`。`dev_release` 的 `deep` 模式可选 `cargo-audit`。

## License

PolyForm Noncommercial License 1.0.0，见 `LICENSE`。派生自上游的部分沿用上游的 license。
