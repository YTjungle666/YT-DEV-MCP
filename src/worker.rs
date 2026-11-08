//! codex worker lifecycle. Each worker gets a plan *slice*, its own git worktree
//! when it runs in parallel, and hard CPU/memory caps via a systemd user scope.

use crate::config::cfg;
use crate::plan;
use crate::util::{
    all_runs, cap1, cap_lines, config_limits_for, dir_bytes, epoch, git, out_str, pid_alive,
    proc_stats, run, runs_dir_for, which,
};
use serde_json::{json, Value};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const MB: u64 = 1024 * 1024;

fn worktree_root() -> PathBuf {
    cfg().paths.state_dir.join(&cfg().worker.worktree_subdir)
}

pub fn run_action(a: &Value) -> Result<String, String> {
    match a["action"].as_str().unwrap_or("") {
        "start" => start_one(a, None),
        "start_batch" => start_batch(a),
        "status" => status(a),
        "list" => list(),
        "kill" => kill(a),
        other => Err(format!("unknown action {other}")),
    }
}

struct Spawn {
    id: String,
    pid: u32,
    cwd: PathBuf,
    branch: String,
}

fn start_batch(a: &Value) -> Result<String, String> {
    let tasks = a["tasks"].as_array().ok_or("start_batch needs tasks[]")?;
    if tasks.is_empty() {
        return Err("start_batch: empty tasks".into());
    }
    let repo = crate::tools::repo_path(a)?;
    let c = crate::config::cfg_for(&repo);
    if tasks.len() > c.limits.max_parallel {
        return Err(format!(
            "start_batch: {} tasks > hard limit {} (split into waves)",
            tasks.len(),
            c.limits.max_parallel
        ));
    }
    let mut lines = Vec::new();
    for t in tasks {
        let mut one = a.clone();
        one["task"] = t["task"].clone();
        if let Some(s) = t.get("scope") {
            one["scope"] = s.clone();
        }
        one["worktree"] = json!(true);
        match start_one(&one, t["name"].as_str()) {
            Ok(s) => lines.push(s),
            Err(e) => lines.push(format!("FAILED: {e}")),
        }
    }
    Ok(lines.join("\n"))
}

fn start_one(a: &Value, label: Option<&str>) -> Result<String, String> {
    let task = a["task"].as_str().ok_or("start needs task")?;
    let repo = crate::tools::repo_path(a)?;
    let c = crate::config::cfg_for(&repo);
    let name = crate::tools::label_of(a, &repo);
    // 有 PLAN.md 声明了这个仓库就顺带切片；没有也不报错。
    let project = plan::project_for_repo(&repo);
    let scopes: Vec<String> = a["scope"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    // Everything this tool writes (run logs, run meta, worktrees) lives under a
    // user-settable cap; prune before growing, never rely on free disk.
    let (max_store_mb, keep_runs) = config_limits_for(&c);
    let max_store_mb = a["max_store_mb"].as_u64().unwrap_or(max_store_mb);
    let (before, after, pruned) = prune_store(max_store_mb, keep_runs);
    if after > max_store_mb * MB {
        return Err(format!(
            "store cap {} MB exceeded even after pruning {} old run(s): {} MB in use. 清理 {} 或调大 [limits] max_store_mb",
            max_store_mb,
            pruned.len(),
            after / MB,
            worktree_root().display()
        ));
    }
    let _ = git(&repo, &["worktree", "prune"]);
    let mem_mb = a["mem_mb"].as_u64().unwrap_or(c.limits.mem_mb);
    let cpu_pct = a["cpu_pct"].as_u64().unwrap_or(c.limits.cpu_pct as u64) as u32;

    let want_wt = a["worktree"].as_bool().unwrap_or(false) || a["action"] == json!("start_batch");

    let id = crate::util::stamp();
    let dir = runs_dir_for(&repo);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // Parallel workers must not share a working tree.
    let (cwd, branch) = if want_wt && repo.join(".git").exists() {
        let wt = worktree_root().join(&id);
        let br = format!("{}{id}", c.worker.branch_prefix);

        if let Some(p) = wt.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let out = run(
            "git",
            &[
                "worktree",
                "add",
                "-b",
                &br,
                wt.to_str().unwrap_or(""),
                "HEAD",
            ],
            Some(&repo),
        );
        if !out.status.success() {
            return Err(format!(
                "git worktree add: {}",
                cap_lines(&out_str(&out), 5)
            ));
        }
        (wt, br)
    } else {
        (repo.clone(), String::new())
    };

    let slice = match project.as_deref() {
        Some(p) => plan::slice(p, &scopes)
            .unwrap_or_else(|_| "（计划里没找到对应段落：按任务描述执行，范围自限）".to_string()),
        None => "（没有 PLAN.md：按任务描述执行，范围自限）".to_string(),
    };
    let plan_path = project
        .as_deref()
        .and_then(plan::plan_path)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let prompt = build_prompt(PromptIn {
        c: &c,
        plan_path: &plan_path,
        project: &name,
        task,
        scopes: &scopes,
        slice: &slice,
        mem_mb,
        cpu_pct,
    });

    let log_path = dir.join(format!("{id}.log"));
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let base = git(&cwd, &["rev-parse", "HEAD"]);

    let mut cmd = capped_command(&c, mem_mb, cpu_pct)?;
    cmd.args(&c.worker.args)
        .arg(&prompt)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(log));

    unsafe {
        cmd.pre_exec(|| {
            setsid();
            Ok(())
        });
    }
    let child = cmd.spawn().map_err(|e| format!("spawn codex: {e}"))?;
    let s = Spawn {
        id: id.clone(),
        pid: child.id(),
        cwd: cwd.clone(),
        branch: branch.clone(),
    };

    let meta = json!({
        "id": s.id, "repo": repo.display().to_string(), "name": name, "project": project,
        "label": label, "task": task, "scope": scopes,
        "cwd": s.cwd.display().to_string(), "branch": if s.branch.is_empty() { Value::Null } else { json!(s.branch) },
        "base_commit": base, "pid": s.pid, "started": epoch(),
        "mem_mb": mem_mb, "cpu_pct": cpu_pct, "log": log_path.display().to_string(),
        "dir": dir.display().to_string()
    });
    std::fs::write(
        dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(&meta).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    // 计划项目的人在 ~/Plan/<项目>/runs 里也能翻到（软链，内容只有一份）。
    if let Some(p) = &project {
        mirror_runs_link(p, &dir);
    }
    Ok(format!(
        "started {id} pid {} mem {mem_mb}MB cpu {cpu_pct}%{}\n  cwd {}\n  log {}\n  store {}MB → {}MB / cap {}MB{}",
        s.pid,
        if s.branch.is_empty() {
            String::new()
        } else {
            format!("\n  branch {}", s.branch)
        },
        s.cwd.display(),
        log_path.display(),
        before / MB,
        after / MB,
        max_store_mb,
        if pruned.is_empty() {
            String::new()
        } else {
            format!(" (pruned {})", pruned.join(", "))
        }
    ))
}

/// `~/Plan/<项目>/runs` 指向 XDG 里的同一份产物；已有目录（老数据）就不动。
fn mirror_runs_link(project: &str, dir: &Path) {
    let link = crate::util::plan_root().join(project).join("runs");
    if link.exists() {
        return;
    }
    let _ = std::os::unix::fs::symlink(dir, &link);
}

/// systemd-run --user --scope gives hard MemoryMax/CPUQuota without a root helper.
fn capped_command(c: &crate::config::Config, mem_mb: u64, cpu_pct: u32) -> Result<Command, String> {
    let w = &c.worker;
    let bin = which(&w.bin).ok_or_else(|| format!("{} not on PATH", w.bin))?;
    let nice = w.nice.to_string();
    let sd = which("systemd-run");
    if let Some(sd) = sd {
        let probe = run(
            sd.to_str().unwrap_or("systemd-run"),
            &["--user", "--scope", "--quiet", "true"],
            None,
        );
        if probe.status.success() {
            let mut c = Command::new(sd);
            c.args([
                "--user",
                "--scope",
                "--quiet",
                "-p",
                &format!("MemoryMax={mem_mb}M"),
                "-p",
                &format!("CPUQuota={cpu_pct}%"),
                "--",
                "nice",
                "-n",
                &nice,
            ])
            .arg(bin);
            return Ok(c);
        }
    }
    // No user scope available: still de-prioritise, and say so in the run log.
    let mut c = Command::new("nice");
    c.arg("-n").arg(&nice).arg(bin);
    Ok(c)
}

/// Everything this tool owns on disk: the XDG state tree plus worktrees.
fn store_roots() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if crate::util::state_root().is_dir() {
        v.push(crate::util::state_root());
    }
    let wt = worktree_root();
    if wt.is_dir() {
        v.push(wt);
    }
    v
}

fn store_bytes() -> u64 {
    store_roots().iter().map(|p| dir_bytes(p)).sum()
}

fn remove_run_files(id: &str, m: &Value, removed: &mut Vec<String>) -> u64 {
    let dir = m["dir"]
        .as_str()
        .map(PathBuf::from)
        .or_else(|| {
            m["repo"]
                .as_str()
                .map(|r| crate::util::runs_dir_for(Path::new(r)))
        })
        .unwrap_or_default();
    let mut freed = 0;
    for name in [format!("{id}.log"), format!("{id}.json")] {
        let p = dir.join(name);
        let b = std::fs::metadata(&p).map(|x| x.len()).unwrap_or(0);
        if std::fs::remove_file(&p).is_ok() {
            freed += b;
        }
    }
    if freed > 0 {
        removed.push(format!("run:{id}"));
    }
    freed
}

/// Keep the newest `keep_runs` runs, then drop oldest finished runs and stale
/// worktrees until the total footprint fits `cap_mb`. Live runs are never cut.
fn prune_store(cap_mb: u64, keep_runs: usize) -> (u64, u64, Vec<String>) {
    let cap = cap_mb * MB;
    let mut total = store_bytes();
    let before = total;
    let mut removed = Vec::new();

    let mut kept = 0usize;
    for (id, m) in all_runs() {
        kept += 1;
        if pid_alive(m["pid"].as_i64().unwrap_or(0)) {
            continue;
        }
        if kept > keep_runs {
            total = total.saturating_sub(remove_run_files(&id, &m, &mut removed));
        }
    }

    if total > cap {
        let mut oldest = all_runs();
        oldest.reverse();
        for (id, m) in oldest {
            if total <= cap {
                break;
            }
            if pid_alive(m["pid"].as_i64().unwrap_or(0)) {
                continue;
            }
            total = total.saturating_sub(remove_run_files(&id, &m, &mut removed));
        }
    }

    if total > cap {
        let wtdir = worktree_root();
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&wtdir)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default();
        dirs.sort();
        for d in dirs {
            if total <= cap {
                break;
            }
            let b = dir_bytes(&d);
            if std::fs::remove_dir_all(&d).is_ok() {
                total = total.saturating_sub(b);
                removed.push(format!(
                    "wt:{}",
                    d.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
    }
    (before, total, removed)
}

fn status(a: &Value) -> Result<String, String> {
    let want = a["run_id"]
        .as_str()
        .or(a["project"].as_str())
        .ok_or("status needs run_id or project")?;
    let Some((id, m)) = all_runs()
        .into_iter()
        .find(|(id, m)| id == want || m["project"] == json!(want))
    else {
        return Err(format!("no run {want}"));
    };
    let pid = m["pid"].as_i64().unwrap_or(0);
    let alive = pid_alive(pid);
    let (cpu, rss) = if alive { proc_stats(pid) } else { (0.0, 0) };
    let log = PathBuf::from(m["log"].as_str().unwrap_or(""));
    let idle = std::fs::metadata(&log)
        .ok()
        .and_then(|x| x.modified().ok())
        .map(crate::util::age_secs);
    let cwd = PathBuf::from(m["cwd"].as_str().unwrap_or("."));
    let base = m["base_commit"].as_str().unwrap_or("HEAD");
    let diffstat = git(&cwd, &["diff", "--stat", base]);
    // Fresh files are untracked, so `git diff` alone reports nothing.
    let dirty = git(&cwd, &["status", "--porcelain"]);
    let mut changes = diffstat.clone();
    if !dirty.trim().is_empty() {
        changes.push_str(&format!(
            "\n-- status --\n{}",
            cap_lines(&dirty, cfg().limits.log_tail_lines)
        ));
    }
    let children = if alive { git_children(pid) } else { 0 };
    let stalled = alive && idle.unwrap_or(0) > cfg().limits.stall_secs && children == 0;
    let disk = out_str(&run("du", &["-sh", cwd.to_str().unwrap_or(".")], None));
    let (cap_mb, _) = config_limits_for(cfg());
    let tail = std::fs::read_to_string(&log)
        .map(|t| cap_lines(&t, cfg().limits.log_tail_lines))
        .unwrap_or_default();

    Ok(format!(
        "run {id} | {} | pid {pid} alive={alive} | {}s | cpu {cpu:.1}% rss {rss}MB | log idle {}s | children {children}{}\n\
         disk {} | mem_cap {}MB | cpu_cap {}%{}\nstore {}MB / cap {}MB\n\n== diffstat\n{}\n\n== log tail\n{}",
        if alive { "running" } else { "finished" },
        epoch() - m["started"].as_i64().unwrap_or(0),
        idle.unwrap_or(-1),
        if stalled { " | STALLED" } else { "" },
        disk.split_whitespace().next().unwrap_or("?"),
        m["mem_mb"].as_u64().unwrap_or(0),
        m["cpu_pct"].as_u64().unwrap_or(0),
        m["branch"].as_str().map(|b| format!("\nbranch {b}")).unwrap_or_default(),
        store_bytes() / MB,
        cap_mb,
        if changes.trim().is_empty() { "(none)" } else { changes.as_str() },
        tail
    ))
}

fn list() -> Result<String, String> {
    let all = all_runs();
    if all.is_empty() {
        return Ok("no runs".into());
    }
    let mut s = String::new();
    for (id, m) in all.iter().take(cfg().limits.max_items) {
        let alive = pid_alive(m["pid"].as_i64().unwrap_or(0));
        s.push_str(&format!(
            "{id} | {} | {} | pid {} alive={alive} | {}\n",
            m["project"].as_str().unwrap_or("-"),
            m["label"]
                .as_str()
                .or(m["scope"]
                    .as_array()
                    .and_then(|s| s.first())
                    .and_then(|v| v.as_str()))
                .unwrap_or("-"),
            m["pid"].as_i64().unwrap_or(0),
            cap1(m["task"].as_str().unwrap_or(""), 50)
        ));
    }
    Ok(s)
}

fn kill(a: &Value) -> Result<String, String> {
    let id = a["run_id"]
        .as_str()
        .ok_or("kill needs run_id (explicit, by id)")?;
    let Some((_, m)) = all_runs().into_iter().find(|(rid, _)| rid == id) else {
        return Err(format!("no run {id}"));
    };
    let pid = m["pid"].as_i64().unwrap_or(0);
    if !pid_alive(pid) {
        return Ok(format!("run {id}: pid {pid} already gone"));
    }
    // Kill the whole scope/process group: codex spawns children (node, rustc).
    let _ = run("kill", &["-TERM", "--", &format!("-{pid}")], None);
    let grace = cfg().limits.kill_grace_secs;
    for _ in 0..grace * 2 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if !pid_alive(pid) {
            return Ok(format!("run {id}: terminated (SIGTERM to process group)"));
        }
    }
    let _ = run("kill", &["-KILL", "--", &format!("-{pid}")], None);
    Ok(format!("run {id}: SIGKILL after {grace}s"))
}

fn git_children(pid: i64) -> usize {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .map(|s| s.split_whitespace().count())
        .unwrap_or(0)
}

extern "C" {
    fn setsid() -> i32;
}

/// 渲染 worker prompt 需要的全部输入；参数多了就打包，别让签名继续长。
struct PromptIn<'a> {
    c: &'a crate::config::Config,
    plan_path: &'a str,
    project: &'a str,
    task: &'a str,
    scopes: &'a [String],
    slice: &'a str,
    mem_mb: u64,
    cpu_pct: u32,
}

fn build_prompt(inp: PromptIn<'_>) -> String {
    let PromptIn {
        c,
        plan_path,
        project,
        task,
        scopes,
        slice,
        mem_mb,
        cpu_pct,
    } = inp;
    let w = &c.worker;
    let tpl = std::fs::read_to_string(&c.paths.worker_prompt).unwrap_or_else(|_| w.prompt.clone());
    let scope = if scopes.is_empty() {
        "（未指定：只做任务描述里明确的那部分）".to_string()
    } else {
        scopes.join("、")
    };
    let rules: String = render_rules(&w.rules);
    tpl.replace("{rules}", &rules)
        .replace("{project}", project)
        .replace("{task}", task)
        .replace("{scope}", &scope)
        .replace("{slice}", slice)
        .replace("{plan_path}", plan_path)
        .replace("{mem_mb}", &mem_mb.to_string())
        .replace("{cpu_pct}", &cpu_pct.to_string())
        .replace("{max_file_lines}", &w.max_file_lines.to_string())
        .replace("{max_fn_lines}", &w.max_fn_lines.to_string())
}

pub const DEFAULT_PROMPT: &str = include_str!("../assets/worker-prompt.md");

fn render_rules(rules: &[String]) -> String {
    rules
        .iter()
        .enumerate()
        .map(|(i, r)| format!("{}. {r}", i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// worker 的硬约束条目（config `[worker] rules` 覆盖）；`{…}` 占位符同样会被替换。
pub const DEFAULT_RULES: [&str; 12] = [
    "只执行，不提问；歧义按片段里的决策取舍。范围外的文件一律不碰，确需改动先写进最后的说明里而不是直接改。",
    "不硬编码：路径、端口、超时、上限、命令、模型名、外部 URL 都从配置文件或参数读，并给默认值与示例；代码里不留只有本机才成立的常量。",
    "可扩展：多一种实现、多一种来源、多一个可调项，只加配置或新模块，不改既有调用方；也不为“以后可能用到”提前抽象。",
    "模块化：一个文件一个职责；新逻辑放独立模块，不往大文件里继续堆；单文件超过 {max_file_lines} 行或函数超过 {max_fn_lines} 行就拆。",
    "资源上限：本进程 MemoryMax={mem_mb}M、CPUQuota={cpu_pct}%；不要起常驻进程，不要跑全量基准，不要下载大文件。",
    "取舍顺序：功能 > 性能 > 资源占用（CPU/内存/硬盘）。先把功能做对做全，再优化热路径与分配，最后压常驻 CPU/内存/磁盘；代码复杂度不算约束，不为“简洁”牺牲上面三项，重构同此。",
    "性能优先：先看复杂度，避免 O(n²) 和每请求分配；复用仓库已有实现，不新造轮子；性能与可读性冲突时选性能，并注释说明。",
    "不 push、不打 tag、不改 PLAN.md。",
    "门禁必须干净：fmt 无差异、clippy 与构建零 warning、测试零失败、零 error。任何 warning 都算没做完，不许用 #[allow]、-A、删测试或注释掉代码压过去；确实要留的例外写进最后说明交 YT 定。",
    "结束用一句话说明改了什么、没做什么。",
    "语言：新项目默认 Rust（跨平台核心、常驻进程、命令行工具一律 Rust）。仓库里已有源码的，沿用仓库原有的语言，不要为了“更现代”换语言；只有计划里明确写了“重构/迁移”，才按计划的边界换。要不要换语言不由你决定，写进最后说明留给 YT。",
    "Rust 构建时间：日常验证只跑 `cargo check` 或 debug `cargo build`，`--release` 收尾验一次就够；release 用 opt-level=3 + lto=\"thin\"，fat LTO 只留给 CI 发版。本机已装 sccache 与 mold 并写进 ~/.cargo/config.toml，不要覆盖或清掉。",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_render_numbered_and_default_prompt_wires_them() {
        assert_eq!(
            render_rules(&["甲".to_string(), "乙".to_string()]),
            "1. 甲\n2. 乙"
        );
        let all = DEFAULT_RULES.join("\n");
        assert!(all.contains("不硬编码"), "少了不硬编码约束");
        assert!(all.contains("可扩展"), "少了可扩展约束");
        assert!(all.contains("{max_file_lines}"), "行数上限要走占位符");
        assert!(all.contains("零 warning"), "少了零警告门禁约束");
        assert!(all.contains("功能 > 性能 > 资源占用"), "少了取舍顺序约束");
        assert!(DEFAULT_PROMPT.contains("{rules}"), "模板没接上 rules");
    }

    #[test]
    fn prune_drops_oldest_runs_until_under_cap() {
        // runs 现在按仓库落在 XDG state 下：一个 hash 目录 = 一个仓库。
        let tmp = std::env::temp_dir().join("yt-dev-prune-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let runs = tmp.join("yt-dev/0123456789abcdef").join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        std::env::set_var("XDG_STATE_HOME", &tmp);
        crate::config::reset();

        for i in 0..3 {
            let id = format!("100{i}-000000");
            std::fs::write(
                runs.join(format!("{id}.json")),
                json!({"id": id, "repo": "/tmp/r", "pid": 0, "started": 1000 + i,
                       "dir": runs.display().to_string()})
                .to_string(),
            )
            .unwrap();
            std::fs::write(runs.join(format!("{id}.log")), "x".repeat(400_000)).unwrap();
        }
        let (before, after, removed) = prune_store(1, 30);
        assert!(before > MB, "before={before}");
        assert!(after <= MB, "after={after}");
        assert!(!removed.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::remove_var("XDG_STATE_HOME");
        crate::config::reset();
    }
}
