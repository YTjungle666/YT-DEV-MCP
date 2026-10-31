//! The seven tools. Each returns plain text (or compact JSON) meant to be read
//! by a model: counts + top items, never unbounded dumps.

use crate::config::cfg;
use crate::plan;
use crate::util::{all_runs, cap_lines, epoch, git, home, out_str, redact, run, which};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn schemas() -> Value {
    let c = cfg();
    let mem = c.limits.mem_mb;
    let cpu = c.limits.cpu_pct;
    let parallel = c.limits.max_parallel;
    let store = c.limits.max_store_mb;
    let gate_timeout = c.limits.gate_timeout_secs;
    let license = c.publish.license.clone();
    json!([
        {
            "name": "dev_projects",
            "description": "列出 ~/Plan 下所有项目的状态行、git 状态（分支/脏文件/未推提交）、活跃 worker 与卡死嫌疑。回答“现在有哪几摊事、哪摊在动”。",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "dev_plan",
            "description": "把某个项目的 PLAN.md 解析成结构化状态：状态行、决策表、阶段进度、验证清单、YT 待办、下一步。结构不符时返回原文而非空值。",
            "inputSchema": {"type": "object", "properties": {"project": {"type": "string", "description": "~/Plan 下的目录名"}}, "required": ["project"]}
        },
        {
            "name": "dev_plan_update",
            "description": "对 PLAN.md 做定点写回（状态行 / 单个勾选框 / 追加 run 记录），返回改动 diff。匹配不唯一时拒写。",
            "inputSchema": {"type": "object", "properties": {
                "project": {"type": "string"},
                "ops": {"type": "array", "items": {"type": "object", "properties": {
                    "kind": {"type": "string", "enum": ["set_status", "check_step", "append_run_note"]},
                    "value": {"type": "string"},
                    "phase": {"type": "string"},
                    "index": {"type": "integer"},
                    "done": {"type": "boolean"},
                    "text": {"type": "string"}
                }, "required": ["kind"]}}
            }, "required": ["project", "ops"]}
        },
        {
            "name": "dev_worker",
            "description": "codex worker 生命周期。一个 worker 只给计划的一个切片（scope），并行任务用 start_batch 各起独立 worktree；CPU/内存有硬上限，kill 只在显式调用时执行。",
            "inputSchema": {"type": "object", "properties": {
                "action": {"type": "string", "enum": ["start", "start_batch", "status", "list", "kill"]},
                "project": {"type": "string"},
                "task": {"type": "string", "description": "这一个 worker 要做的事，越具体越好"},
                "scope": {"type": "array", "items": {"type": "string"}, "description": "该 worker 负责的计划段落关键字（阶段标题），只把这部分拼进 prompt"},
                "tasks": {"type": "array", "items": {"type": "object", "properties": {
                    "name": {"type": "string"}, "task": {"type": "string"}, "scope": {"type": "array", "items": {"type": "string"}}
                }, "required": ["task"]}, "description": format!("start_batch 用：并行任务列表（上限 {parallel}）")},
                "cwd": {"type": "string"},
                "run_id": {"type": "string"},
                "worktree": {"type": "boolean", "description": "用独立 git worktree 隔离（start_batch 强制开启）"},
                "mem_mb": {"type": "integer", "default": mem, "description": "MemoryMax（硬上限）"},
                "cpu_pct": {"type": "integer", "default": cpu, "description": "CPUQuota 百分比（硬上限）"},
                "max_store_mb": {"type": "integer", "description": format!("本次允许的产物占用上限（run 日志/元数据 + worktree），缺省取 config.toml 的 [limits] max_store_mb（当前 {store}）")}

            }, "required": ["action"]}
        },
        {
            "name": "dev_review",
            "description": "对 worker 的改动起独立 review（强模型线），返回 verdict 与按文件的问题清单。",
            "inputSchema": {"type": "object", "properties": {
                "run_id": {"type": "string"},
                "project": {"type": "string"},
                "base": {"type": "string", "description": "对比基线，默认 run 的 base_commit 或 HEAD"}
            }}
        },
        {
            "name": "dev_gate",
            "description": "按仓库形态跑门禁：cargo fmt/clippy/test 或 npm build/test 或 make verify，逐步返回通过/失败与输出尾部。",
            "inputSchema": {"type": "object", "properties": {
                "project": {"type": "string"},
                "scope": {"type": "string", "enum": ["fast", "full"], "default": "fast"},
                "timeout_secs": {"type": "integer", "default": gate_timeout}

            }, "required": ["project"]}
        },
        {
            "name": "dev_release",
            "description": "发布预检：版本一致性、变更摘要、隐私扫描(gitleaks)、依赖漏洞(osv-scanner/cargo audit)、tag 命令预览。默认 dry-run，从不执行发布。",
            "inputSchema": {"type": "object", "properties": {
                "project": {"type": "string"},
                "version": {"type": "string", "description": "目标版本，如 0.1.13"},
                "deep": {"type": "boolean", "default": false, "description": "追加 cargo audit"},
                "dry_run": {"type": "boolean", "default": true}
            }, "required": ["project", "version"]}
        },
        {
            "name": "dev_publish",
            "description": "上传 GitHub 并加保护规则。先校验：未提交改动、README 长度、LICENSE（默认 config.toml 的 [publish] license；派生自上游必须与上游同 license）、gitleaks 密钥扫描；通过后才 gh repo create/push，并给主干加保护（禁强推/禁删除、要求 ci 通过）。默认 dry-run。",
            "inputSchema": {"type": "object", "properties": {
                "project": {"type": "string"},
                "repo": {"type": "string", "description": "owner/name，缺省用 origin remote 或 gh 用户名 + 目录名"},
                "visibility": {"type": "string", "enum": ["public", "private"], "default": "public"},
                "license": {"type": "string", "description": format!("缺省 {license}")},

                "upstream": {"type": "string", "description": "派生来源 owner/name：自动对齐其 license"},
                "dry_run": {"type": "boolean", "default": true}
            }, "required": ["project"]}
        }
    ])
}

pub fn call(params: &Value) -> Result<Value, (i32, String)> {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let a = params.get("arguments").cloned().unwrap_or(json!({}));
    let out = match name {
        "dev_projects" => projects(),
        "dev_plan" => plan::describe(a["project"].as_str().unwrap_or("")).map(|v| v.to_string()),
        "dev_plan_update" => plan::update(
            a["project"].as_str().unwrap_or(""),
            a["ops"].as_array().cloned().unwrap_or_default().as_slice(),
        ),
        "dev_worker" => crate::worker::run_action(&a),
        "dev_review" => review(&a),
        "dev_gate" => gate(&a),
        "dev_release" => release(&a),
        "dev_publish" => crate::publish::run_publish(&a),
        other => Err(format!("unknown tool {other}")),
    };
    Ok(match out {
        Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
        Err(e) => {
            json!({"content": [{"type": "text", "text": format!("error: {e}")}], "isError": true})
        }
    })
}

// ---------------------------------------------------------------- projects

pub(crate) fn repo_of(project: &str) -> Option<PathBuf> {
    // Explicit mapping first, then whatever the plan declares, then ~/<name>.
    let cfg_file = cfg().paths.projects.clone();
    if let Ok(t) = std::fs::read_to_string(&cfg_file) {
        if let Ok(v) = t.parse::<toml::Value>() {
            if let Some(p) = v.get(project).and_then(|x| x.as_str()) {
                return Some(PathBuf::from(p));
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(plan_path_of(project)?) {
        if let Some(p) = crate::util::repo_from_plan(&text) {
            if Path::new(&p).is_dir() {
                return Some(PathBuf::from(p));
            }
        }
    }
    let guess = home().join(project);
    guess.is_dir().then_some(guess)
}

fn plan_path_of(project: &str) -> Option<PathBuf> {
    plan::plan_path(project)
}

fn projects() -> Result<String, String> {
    let mut rows = Vec::new();
    let mut unknown = Vec::new();
    let mut active: Vec<(String, String)> = Vec::new();
    for (id, meta) in all_runs() {
        if meta["status"] == json!("running") {
            active.push((id, meta["project"].as_str().unwrap_or("-").to_string()));
        }
    }
    for p in plan::list_projects() {
        let Some(path) = plan::plan_path(&p) else {
            unknown.push(format!("{p} (no PLAN.md)"));
            continue;
        };
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let parsed = plan::parse(&text);
        let status = parsed.status_line.clone().unwrap_or_else(|| "?".into());
        let age = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(crate::util::age_secs)
            .unwrap_or(0);
        let (repo, branch, dirty, unpushed) = match repo_of(&p) {
            Some(r) if is_git(&r) => {
                let br = git(&r, &["rev-parse", "--abbrev-ref", "HEAD"]);
                let d = git(&r, &["status", "--porcelain"])
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .count();
                let u = git(&r, &["rev-list", "--count", "@{u}..HEAD"]);
                (r.display().to_string(), br, d.to_string(), u)
            }
            Some(r) => (
                format!("{} (not git)", r.display()),
                "-".into(),
                "-".into(),
                "-".into(),
            ),
            None => ("(unresolved)".into(), "-".into(), "-".into(), "-".into()),
        };
        let running: Vec<&str> = active
            .iter()
            .filter(|(_, pr)| pr == &p)
            .map(|(id, _)| id.as_str())
            .collect();
        let gitcol = if branch == "-" {
            "-".to_string()
        } else {
            format!("{branch} {dirty} dirty, {unpushed} unpushed")
        };
        rows.push(format!(
            "{p} | {status} | plan {}d ago | {gitcol} | {repo}{}",
            age / 86400,
            if running.is_empty() {
                String::new()
            } else {
                format!(" | runs: {}", running.join(","))
            }
        ));
    }
    let mut s = String::from("project | status | plan | git | repo\n");
    s.push_str(&rows.join("\n"));
    if !unknown.is_empty() {
        s.push_str(&format!("\n\n未解析: {}", unknown.join("; ")));
    }
    if active.is_empty() {
        s.push_str("\n\nno running workers");
    }
    Ok(s)
}

fn is_git(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// Version can live in `[package]` or `[workspace.package]`.
fn toml_version(t: &str) -> Option<String> {
    let mut in_ver_table = false;
    for l in t.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            in_ver_table = l == "[package]" || l == "[workspace.package]";
            continue;
        }
        if in_ver_table {
            if let Some(v) = l.strip_prefix("version") {
                return Some(
                    v.trim_start_matches([' ', '='])
                        .trim_matches('"')
                        .to_string(),
                );
            }
        }
    }
    None
}

// ------------------------------------------------------------------ review

pub const DEFAULT_REVIEW_PROMPT: &str = include_str!("../assets/review-prompt.md");

fn review(a: &Value) -> Result<String, String> {
    let (repo, base, task) = if let Some(id) = a["run_id"].as_str() {
        let (_, m) = all_runs()
            .into_iter()
            .find(|(rid, _)| rid == id)
            .ok_or(format!("no run {id}"))?;
        (
            PathBuf::from(m["cwd"].as_str().unwrap_or(".")),
            m["base_commit"].as_str().unwrap_or("HEAD").to_string(),
            m["task"].as_str().unwrap_or("").to_string(),
        )
    } else {
        let p = a["project"]
            .as_str()
            .ok_or("review needs run_id or project")?;
        (
            repo_of(p).ok_or(format!("cannot resolve repo for {p}"))?,
            a["base"].as_str().unwrap_or("HEAD").to_string(),
            String::new(),
        )
    };
    let rc = &cfg().review;
    let tpl =
        std::fs::read_to_string(&cfg().paths.review_prompt).unwrap_or_else(|_| rc.prompt.clone());
    let prompt = tpl.replace("{base}", &base).replace("{task}", &task);
    let out_file = std::env::temp_dir().join(format!("yt-dev-review-{}.json", epoch()));
    let out_s = out_file.to_string_lossy().into_owned();
    let args: Vec<String> = rc.args.iter().map(|x| x.replace("{out}", &out_s)).collect();
    let mut cmd = Command::new(which(&rc.bin).ok_or_else(|| format!("{} not on PATH", rc.bin))?);
    cmd.args(&args)
        .arg(&prompt)
        .current_dir(&repo)
        .stdin(Stdio::null());

    let _out = cmd.output().map_err(|e| format!("run codex: {e}"))?;
    // stderr carries the session banner and hook lines; the answer is in the -o file.
    let text = std::fs::read_to_string(&out_file).unwrap_or_else(|_| String::new());
    let _ = std::fs::remove_file(&out_file);
    let json_slice = match (text.find('{'), text.rfind('}')) {
        (Some(s), Some(e)) if e > s => Some(&text[s..=e]),
        _ => None,
    };
    let parsed = json_slice.and_then(|s| serde_json::from_str::<Value>(s).ok());
    match parsed {
        Some(v) => {
            let findings = v["findings"].as_array().cloned().unwrap_or_default();
            let mut s = format!(
                "verdict: {}\nfindings: {}\n",
                v["verdict"].as_str().unwrap_or("?"),
                findings.len()
            );
            for f in findings.iter().take(cfg().limits.max_items) {
                s.push_str(&format!(
                    "- [{}] {}:{} {}\n  → {}\n",
                    f["severity"].as_str().unwrap_or("?"),
                    f["file"].as_str().unwrap_or("-"),
                    f["line"].as_i64().unwrap_or(0),
                    f["issue"].as_str().unwrap_or(""),
                    f["suggestion"].as_str().unwrap_or("")
                ));
            }
            Ok(s)
        }
        None => Ok(format!(
            "parse: failed (expected JSON from reviewer)\n\n{}",
            cap_lines(&text, 30)
        )),
    }
}

// -------------------------------------------------------------------- gate

fn gate(a: &Value) -> Result<String, String> {
    let project = a["project"].as_str().ok_or("gate needs project")?;
    let repo = repo_of(project).ok_or(format!("cannot resolve repo for {project}"))?;
    let full = a["scope"].as_str() == Some("full");
    let timeout = a["timeout_secs"]
        .as_u64()
        .unwrap_or(cfg().limits.gate_timeout_secs);
    let steps = gate_steps(&repo, project, full);
    if steps.is_empty() {
        return Err(format!(
            "no gate matched {} —— 在 {} 的 [gate.profiles] 或 [gate.overrides.\"{project}\"] 里加",
            repo.display(),
            crate::config::file().display()
        ));
    }
    let mut report = String::new();
    for h in cargo_profile_hints(&repo) {
        report.push_str(&format!("hint: {h}\n"));
    }
    let mut failed = 0;
    for (name, argv) in steps {
        let started = std::time::Instant::now();
        let mut cmd = Command::new("timeout");
        cmd.arg(timeout.to_string()).args(&argv).current_dir(&repo);
        let out = cmd.output().map_err(|e| format!("run {argv:?}: {e}"))?;
        let ok = out.status.success();
        if !ok {
            failed += 1;
        }

        report.push_str(&format!(
            "{} {name}: {} ({}s)\n",
            if ok { "PASS" } else { "FAIL" },
            argv.join(" "),
            started.elapsed().as_secs()
        ));
        if !ok {
            report.push_str(&cap_lines(&out_str(&out), 15));
            report.push('\n');
        }
    }
    report.push_str(&format!(
        "\n{} failed / {} steps",
        failed,
        steps_len(&failed, &report)
    ));
    Ok(report)
}

fn steps_len(failed: &i32, report: &str) -> usize {
    let _ = failed;
    report
        .lines()
        .filter(|l| l.starts_with("PASS ") || l.starts_with("FAIL "))
        .count()
}

/// 门禁步骤来自配置：先看 `[gate.overrides.<project>]`，否则按顺序取第一个命中
/// `detect` 文件的 profile。`full_only` 只在 scope=full 时跑，`needs` 要求命中
/// 文件里出现那段字样（如 package.json 有没有 "test"）。
fn gate_steps(repo: &Path, project: &str, full: bool) -> Vec<(String, Vec<String>)> {
    let g = &cfg().gate;
    let (steps, hay) = if let Some(ov) = g.overrides.get(project) {
        (ov.as_slice(), String::new())
    } else {
        let hit = g.profiles.iter().find_map(|p| {
            let f = p
                .detect
                .iter()
                .map(|n| repo.join(n))
                .find(|f| f.is_file())?;
            Some((
                p.steps.as_slice(),
                std::fs::read_to_string(f).unwrap_or_default(),
            ))
        });
        match hit {
            Some((s, t)) => (s, t),
            None => return Vec::new(),
        }
    };
    steps
        .iter()
        .filter(|s| full || !s.full_only)
        .filter(|s| s.needs.as_deref().is_none_or(|n| hay.contains(n)))
        .filter(|s| !s.cmd.is_empty())
        .map(|s| {
            let name = if s.name.is_empty() {
                s.cmd[0].clone()
            } else {
                s.name.clone()
            };
            (name, s.cmd.clone())
        })
        .collect()
}

/// Release-profile settings that silently slow the agent loop down.
fn cargo_profile_hints(repo: &Path) -> Vec<String> {
    let Ok(t) = std::fs::read_to_string(repo.join("Cargo.toml")) else {
        return Vec::new();
    };
    let mut v = Vec::new();
    let fat_lto = t
        .lines()
        .any(|l| matches!(l.trim(), "lto = true" | "lto = \"fat\""));
    let small_opt = t
        .lines()
        .any(|l| matches!(l.trim(), "opt-level = \"z\"" | "opt-level = \"s\""));
    if fat_lto {
        v.push(
            "release 开了 fat LTO：改一行也要全量重链（本机实测 7.8s，关掉 1.2s）。\
             日常用 cargo check/debug，发版才 --release；或改 lto=\"thin\""
                .to_string(),
        );
    }
    if small_opt {
        v.push("opt-level=z/s 是压体积档，运行时比 3 慢（为体积保留则忽略本条）".to_string());
    }
    v
}

// ----------------------------------------------------------------- release

fn release(a: &Value) -> Result<String, String> {
    let project = a["project"].as_str().ok_or("release needs project")?;
    let version = a["version"]
        .as_str()
        .ok_or("release needs version")?
        .trim_start_matches('v')
        .to_string();
    let deep = a["deep"].as_bool().unwrap_or(false);
    let repo = repo_of(project).ok_or(format!("cannot resolve repo for {project}"))?;
    let mut blockers: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // 1. version consistency
    let mut versions = Vec::new();
    let cargo = repo.join("Cargo.toml");
    if cargo.is_file() {
        let t = std::fs::read_to_string(&cargo).unwrap_or_default();
        if let Some(v) = toml_version(&t) {
            let ok = v == version;
            versions.push(format!(
                "Cargo.toml: {v}{}",
                if ok { "" } else { " (mismatch)" }
            ));
            if !ok {
                blockers.push(format!("Cargo.toml version {v} != {version}"));
            }
        }
    }
    let pkg = repo.join("package.json");
    if pkg.is_file() {
        let t = std::fs::read_to_string(&pkg).unwrap_or_default();
        if let Ok(v) = serde_json::from_str::<Value>(&t) {
            if let Some(cur) = v["version"].as_str() {
                versions.push(format!("package.json: {cur}"));
                if cur != version {
                    blockers.push(format!("package.json version {cur} != {version}"));
                }
            }
        }
    }
    if versions.is_empty() {
        warnings.push("no version file found (Cargo.toml/package.json)".into());
    }
    if let Some(pp) = plan::plan_path(project) {
        let text = std::fs::read_to_string(&pp).unwrap_or_default();
        if !text.contains(&version) {
            warnings.push(format!("PLAN.md 状态行未提到 {version}"));
        }
    }

    // 2. changelog draft
    let last_tag = git(&repo, &["describe", "--tags", "--abbrev=0"]);
    let range = if last_tag.is_empty() {
        "HEAD".to_string()
    } else {
        format!("{last_tag}..HEAD")
    };
    let log = git(&repo, &["log", "--oneline", &range]);
    let max = cfg().limits.max_items;
    let commits: Vec<&str> = log.lines().take(max).collect();
    // 3. secrets
    let mut secret_lines = Vec::new();
    if let Some(bin) = which("gitleaks") {
        let tmp = std::env::temp_dir().join(format!("yt-dev-gl-{}.json", epoch()));
        let gl_cfg = cfg().paths.gitleaks_config.clone();
        let max_mb = cfg().limits.gitleaks_max_mb.to_string();
        let mut args = vec![
            "dir",
            repo.to_str().unwrap_or("."),
            "--report-format",
            "json",
            "--no-banner",
            "--exit-code",
            "0",
            "--max-target-megabytes",
            max_mb.as_str(),
            "--report-path",
            tmp.to_str().unwrap_or("/tmp/gl.json"),
        ];
        let cfg_s = gl_cfg.to_str().unwrap_or("").to_string();
        if gl_cfg.is_file() {
            args.extend(["--config", cfg_s.as_str()]);
        }
        let _ = run(bin.to_str().unwrap_or("gitleaks"), &args, None);
        if let Ok(t) = std::fs::read_to_string(&tmp) {
            if let Ok(arr) = serde_json::from_str::<Vec<Value>>(&t) {
                for h in arr.iter().take(max) {
                    secret_lines.push(format!(
                        "[{}] {}:{} {}",
                        h["RuleID"].as_str().unwrap_or("?"),
                        h["File"].as_str().unwrap_or("?"),
                        h["StartLine"].as_i64().unwrap_or(0),
                        redact(h["Match"].as_str().unwrap_or(""))
                    ));
                }
            }
        }
        let _ = std::fs::remove_file(&tmp);
    } else {
        warnings.push("gitleaks missing — secret scan skipped".into());
    }

    // 4. vulnerabilities
    let mut vulns = Vec::new();
    if let Some(bin) = which("osv-scanner") {
        let tmp = std::env::temp_dir().join(format!("yt-dev-osv-{}.json", epoch()));
        let tmp_s = tmp.to_string_lossy().into_owned();
        let repo_s = repo.to_string_lossy().into_owned();
        let args = [
            "scan",
            "source",
            "-r",
            repo_s.as_str(),
            "-f",
            "json",
            "--output-file",
            tmp_s.as_str(),
        ];
        let _ = run(bin.to_str().unwrap_or("osv-scanner"), &args, None);
        if let Ok(t) = std::fs::read_to_string(&tmp) {
            if let Ok(v) = serde_json::from_str::<Value>(&t) {
                for r in v["results"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .take(max)
                {
                    for p in r["packages"].as_array().cloned().unwrap_or_default() {
                        for vuln in p["vulnerabilities"].as_array().cloned().unwrap_or_default() {
                            vulns.push(format!(
                                "{} {} — {}",
                                p["package"]["name"].as_str().unwrap_or("?"),
                                p["package"]["version"].as_str().unwrap_or("?"),
                                vuln["id"].as_str().unwrap_or("?")
                            ));
                        }
                    }
                }
            }
        }
        let _ = std::fs::remove_file(&tmp);
    } else {
        warnings.push("osv-scanner missing — dependency scan skipped".into());
    }
    if deep && which("cargo-audit").is_some() && repo.join("Cargo.lock").is_file() {
        let o = run("cargo", &["audit", "--quiet"], Some(&repo));
        let text = out_str(&o);
        for l in text
            .lines()
            .filter(|l| l.contains("ID:") || l.contains("error"))
            .take(max)
        {
            vulns.push(format!("cargo-audit: {}", l.trim()));
        }
    }

    // 5. tag preview (never executed)
    let tag = format!("v{version}");

    let mut s = String::new();
    s.push_str(&format!("== {project} → {tag} (dry run)\n\n版本:\n"));
    for v in &versions {
        s.push_str(&format!("  {v}\n"));
    }
    s.push_str(&format!("\n变更 ({range}):\n"));
    if commits.is_empty() {
        s.push_str("  (无新提交)\n");
    }
    for c in &commits {
        s.push_str(&format!("  {c}\n"));
    }
    s.push_str(&format!("\n隐私扫描命中: {}\n", secret_lines.len()));
    for l in &secret_lines {
        s.push_str(&format!("  {l}\n"));
    }
    s.push_str(&format!("\n依赖漏洞: {}\n", vulns.len()));
    for v in &vulns {
        s.push_str(&format!("  {v}\n"));
    }
    s.push_str(&format!("\nblockers: {}\n", blockers.len()));
    for b in &blockers {
        s.push_str(&format!("  - {b}\n"));
    }
    s.push_str(&format!("warnings: {}\n", warnings.len()));
    for w in &warnings {
        s.push_str(&format!("  - {w}\n"));
    }
    s.push_str(&format!("\n将执行（未执行）:\n  git tag -a {tag} -m \"{project} {version}\"\n  git push origin {tag}\n"));
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_steps_come_from_config_and_honour_full_only() {
        let dir = std::env::temp_dir().join("yt-dev-gate-steps-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let fast = gate_steps(&dir, "yt-dev-gate-probe", false);
        assert!(
            fast.iter().any(|(n, _)| n == "fmt"),
            "默认应有 rust profile"
        );
        assert!(!fast.iter().any(|(n, _)| n == "build-release"));
        let full = gate_steps(&dir, "yt-dev-gate-probe", true);
        assert!(full.iter().any(|(n, _)| n == "build-release"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_hints_flag_fat_lto_and_opt_z() {
        let dir = std::env::temp_dir().join("yt-dev-profile-hint-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[profile.release]\nopt-level = \"z\"\nlto = true\n",
        )
        .unwrap();
        assert_eq!(cargo_profile_hints(&dir).len(), 2);
        std::fs::write(
            dir.join("Cargo.toml"),
            "[profile.release]\nopt-level = 3\nlto = \"thin\"\n",
        )
        .unwrap();
        assert!(cargo_profile_hints(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
