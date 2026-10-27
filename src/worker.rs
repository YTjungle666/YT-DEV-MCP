//! codex worker lifecycle. Each worker gets a plan *slice*, its own git worktree
//! when it runs in parallel, and hard CPU/memory caps via a systemd user scope.

use crate::plan;
use crate::tools::repo_of;
use crate::util::{
    all_runs, cap1, cap_lines, config_dir, epoch, free_bytes, git, home, out_str, pid_alive,
    proc_stats, run, runs_dir, which,
};
use serde_json::{json, Value};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DEFAULT_MEM_MB: u64 = 4096;
const DEFAULT_CPU_PCT: u32 = 50;
const MAX_PARALLEL: usize = 6;
const MIN_FREE_GB: u64 = 5;

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
    if tasks.len() > MAX_PARALLEL {
        return Err(format!(
            "start_batch: {} tasks > hard limit {MAX_PARALLEL} (split into waves)",
            tasks.len()
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
    let project = a["project"].as_str().ok_or("start needs project")?;
    let task = a["task"].as_str().ok_or("start needs task")?;
    let scopes: Vec<String> = a["scope"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let repo = match a["cwd"].as_str() {
        Some(c) => PathBuf::from(c),
        None => {
            repo_of(project).ok_or("cannot resolve repo (add ~/.config/yt-dev/projects.toml)")?
        }
    };
    if free_bytes(&repo) < MIN_FREE_GB * 1024 * 1024 * 1024 {
        return Err(format!(
            "disk guard: <{MIN_FREE_GB}GB free on {}",
            repo.display()
        ));
    }
    let mem_mb = a["mem_mb"].as_u64().unwrap_or(DEFAULT_MEM_MB);
    let cpu_pct = a["cpu_pct"].as_u64().unwrap_or(DEFAULT_CPU_PCT as u64) as u32;
    let want_wt = a["worktree"].as_bool().unwrap_or(false) || a["action"] == json!("start_batch");

    let id = crate::util::stamp();
    let dir = runs_dir(project);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // Parallel workers must not share a working tree.
    let (cwd, branch) = if want_wt && repo.join(".git").exists() {
        let wt = home().join(".cache/yt-dev/wt").join(&id);
        let br = format!("yt-dev/{id}");
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

    let slice = plan::slice(project, &scopes).unwrap_or_else(|e| format!("(切片失败: {e})"));
    let plan_path = plan::plan_path(project)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let prompt = build_prompt(&plan_path, project, task, &scopes, &slice, mem_mb, cpu_pct);

    let log_path = dir.join(format!("{id}.log"));
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let base = git(&cwd, &["rev-parse", "HEAD"]);

    let mut cmd = capped_command(mem_mb, cpu_pct)?;
    cmd.args([
        "exec",
        "--dangerously-bypass-approvals-and-sandbox",
        &prompt,
    ])
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
        "id": s.id, "project": project, "label": label, "task": task, "scope": scopes,
        "cwd": s.cwd.display().to_string(), "branch": if s.branch.is_empty() { Value::Null } else { json!(s.branch) },
        "base_commit": base, "pid": s.pid, "started": epoch(),
        "mem_mb": mem_mb, "cpu_pct": cpu_pct, "log": log_path.display().to_string()
    });
    std::fs::write(
        dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(&meta).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    Ok(format!(
        "started {id} pid {} mem {mem_mb}MB cpu {cpu_pct}%{}\n  cwd {}\n  log {}",
        s.pid,
        if s.branch.is_empty() {
            String::new()
        } else {
            format!("\n  branch {}", s.branch)
        },
        s.cwd.display(),
        log_path.display()
    ))
}

/// systemd-run --user --scope gives hard MemoryMax/CPUQuota without a root helper.
fn capped_command(mem_mb: u64, cpu_pct: u32) -> Result<Command, String> {
    let codex = which("codex").ok_or("codex not on PATH")?;
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
                "10",
            ])
            .arg(codex);
            return Ok(c);
        }
    }
    // No user scope available: still de-prioritise, and say so in the run log.
    let mut c = Command::new("nice");
    c.arg("-n").arg("10").arg(codex);
    Ok(c)
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
        changes.push_str(&format!("\n-- status --\n{}", cap_lines(&dirty, 12)));
    }
    let children = if alive { git_children(pid) } else { 0 };
    let stalled = alive && idle.unwrap_or(0) > 300 && children == 0;
    let disk = out_str(&run("du", &["-sh", cwd.to_str().unwrap_or(".")], None));
    let tail = std::fs::read_to_string(&log)
        .map(|t| cap_lines(&t, 30))
        .unwrap_or_default();
    Ok(format!(
        "run {id} | {} | pid {pid} alive={alive} | {}s | cpu {cpu:.1}% rss {rss}MB | log idle {}s | children {children}{}\n\
         disk {} | mem_cap {}MB | cpu_cap {}%{}\n\n== diffstat\n{}\n\n== log tail\n{}",
        if alive { "running" } else { "finished" },
        epoch() - m["started"].as_i64().unwrap_or(0),
        idle.unwrap_or(-1),
        if stalled { " | STALLED" } else { "" },
        disk.split_whitespace().next().unwrap_or("?"),
        m["mem_mb"].as_u64().unwrap_or(0),
        m["cpu_pct"].as_u64().unwrap_or(0),
        m["branch"].as_str().map(|b| format!("\nbranch {b}")).unwrap_or_default(),
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
    for (id, m) in all.iter().take(20) {
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
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if !pid_alive(pid) {
            return Ok(format!("run {id}: terminated (SIGTERM to process group)"));
        }
    }
    let _ = run("kill", &["-KILL", "--", &format!("-{pid}")], None);
    Ok(format!("run {id}: SIGKILL after 10s"))
}

fn git_children(pid: i64) -> usize {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .map(|s| s.split_whitespace().count())
        .unwrap_or(0)
}

extern "C" {
    fn setsid() -> i32;
}

fn build_prompt(
    plan_path: &str,
    project: &str,
    task: &str,
    scopes: &[String],
    slice: &str,
    mem_mb: u64,
    cpu_pct: u32,
) -> String {
    let tpl = std::fs::read_to_string(config_dir().join("worker-prompt.md"))
        .unwrap_or_else(|_| DEFAULT_PROMPT.into());
    let scope = if scopes.is_empty() {
        "（未指定：只做任务描述里明确的那部分）".to_string()
    } else {
        scopes.join("、")
    };
    tpl.replace("{project}", project)
        .replace("{task}", task)
        .replace("{scope}", &scope)
        .replace("{slice}", slice)
        .replace("{plan_path}", plan_path)
        .replace("{mem_mb}", &mem_mb.to_string())
        .replace("{cpu_pct}", &cpu_pct.to_string())
}

const DEFAULT_PROMPT: &str = "项目：{project}\n完整计划：{plan_path}（只在需要查证时读，不要通读全篇）\n\n你只负责这一部分：{scope}\n\n计划片段（决策已定，不要重新讨论方案）：\n{slice}\n\n任务：{task}\n\n硬约束：\n1. 只执行，不提问；歧义按片段里的决策取舍。范围外的文件一律不碰，确需改动先写进最后的说明里而不是直接改。\n2. 模块化：一个文件一个职责；新逻辑放独立模块，不往大文件里继续堆；单文件超过 400 行或函数超过 60 行就拆。\n3. 资源上限：本进程 MemoryMax={mem_mb}M、CPUQuota={cpu_pct}%；不要起常驻进程，不要跑全量基准，不要下载大文件。\n4. 性能优先：先看复杂度，避免 O(n²) 和每请求分配；复用仓库已有实现，不新造轮子；性能与可读性冲突时选性能，并注释说明。\n5. 不 push、不打 tag、不改 PLAN.md。\n6. 结束用一句话说明改了什么、没做什么。\n";

#[allow(dead_code)]
fn _unused(_: &Path) {}
