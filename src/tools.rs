//! The seven tools. Each returns plain text (or compact JSON) meant to be read
//! by a model: counts + top items, never unbounded dumps.

use crate::plan;
use crate::util::{all_runs, cap_lines, config_dir, epoch, git, home, out_str, redact, run, which};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const MAX_ITEMS: usize = 20;

pub fn schemas() -> Value {
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
                }, "required": ["task"]}, "description": "start_batch 用：并行任务列表（上限 6）"},
                "cwd": {"type": "string"},
                "run_id": {"type": "string"},
                "worktree": {"type": "boolean", "description": "用独立 git worktree 隔离（start_batch 强制开启）"},
                "mem_mb": {"type": "integer", "default": 4096, "description": "MemoryMax（硬上限）"},
                "cpu_pct": {"type": "integer", "default": 50, "description": "CPUQuota 百分比（硬上限）"},
                "max_store_mb": {"type": "integer", "description": "本次允许的产物占用上限（run 日志/元数据 + worktree），缺省取 ~/.config/yt-dev/config.toml 的 [limits] max_store_mb（默认 4096）"}
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
                "timeout_secs": {"type": "integer", "default": 600}
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
            "description": "上传 GitHub 并加保护规则。先校验：未提交改动、README 长度、LICENSE（默认 PolyForm Noncommercial 1.0.0；派生自上游必须与上游同 license）、gitleaks 密钥扫描；通过后才 gh repo create/push，并给 main 加保护（禁强推/禁删除、要求 ci 通过）。默认 dry-run。",
            "inputSchema": {"type": "object", "properties": {
                "project": {"type": "string"},
                "repo": {"type": "string", "description": "owner/name，缺省用 origin remote 或 gh 用户名 + 目录名"},
                "visibility": {"type": "string", "enum": ["public", "private"], "default": "public"},
                "license": {"type": "string", "description": "缺省 PolyForm-Noncommercial-1.0.0"},
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
    let cfg = config_dir().join("projects.toml");
    if let Ok(t) = std::fs::read_to_string(&cfg) {
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
    let prompt = format!(
        "你是独立评审者。只评审 `git diff {base}` 的改动（必要时读相关文件）。任务背景：{task}\n\
         只输出 JSON，不要任何解释或代码块围栏：\n\
         {{\"verdict\":\"pass|needs-work|fail\",\"findings\":[{{\"file\":\"...\",\"line\":0,\"severity\":\"high|medium|low\",\"issue\":\"...\",\"suggestion\":\"...\"}}]}}\n\
         没有问题时 findings 为空数组。不要修改任何文件。"
    );
    let mut cmd = Command::new(which("codex").ok_or("codex not on PATH")?);
    let out_file = std::env::temp_dir().join(format!("yt-dev-review-{}.json", epoch()));
    let out_s = out_file.to_string_lossy().into_owned();
    cmd.args([
        "exec",
        "--profile",
        "strong",
        "--dangerously-bypass-approvals-and-sandbox",
        "-o",
        out_s.as_str(),
        &prompt,
    ])
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
            for f in findings.iter().take(MAX_ITEMS) {
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
    let timeout = a["timeout_secs"].as_u64().unwrap_or(600);
    let mut steps: Vec<(&str, Vec<&str>)> = Vec::new();
    if repo.join("Cargo.toml").is_file() {
        steps.push(("fmt", vec!["cargo", "fmt", "--check"]));
        steps.push((
            "clippy",
            vec!["cargo", "clippy", "--all-targets", "--", "-D", "warnings"],
        ));
        steps.push(("test", vec!["cargo", "test"]));
        if full {
            steps.push(("build-release", vec!["cargo", "build", "--release"]));
        }
    } else if repo.join("package.json").is_file() {
        let pkg = std::fs::read_to_string(repo.join("package.json")).unwrap_or_default();
        if pkg.contains("\"build\"") {
            steps.push(("build", vec!["npm", "run", "build"]));
        }
        if pkg.contains("\"test\"") {
            steps.push(("test", vec!["npm", "test"]));
        }
    } else if repo.join("Makefile").is_file()
        && std::fs::read_to_string(repo.join("Makefile"))
            .unwrap_or_default()
            .contains("verify:")
    {
        steps.push(("verify", vec!["make", "verify"]));
    }
    if steps.is_empty() {
        return Err(format!(
            "no gate detected in {} (Cargo.toml/package.json/make verify)",
            repo.display()
        ));
    }
    let mut report = String::new();
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
    let commits: Vec<&str> = log.lines().take(MAX_ITEMS).collect();
    // 3. secrets
    let mut secret_lines = Vec::new();
    if let Some(bin) = which("gitleaks") {
        let tmp = std::env::temp_dir().join(format!("yt-dev-gl-{}.json", epoch()));
        let cfg = config_dir().join("gitleaks.toml");
        let mut args = vec![
            "dir",
            repo.to_str().unwrap_or("."),
            "--report-format",
            "json",
            "--no-banner",
            "--exit-code",
            "0",
            "--max-target-megabytes",
            "5",
            "--report-path",
            tmp.to_str().unwrap_or("/tmp/gl.json"),
        ];
        let cfg_s = cfg.to_str().unwrap_or("").to_string();
        if cfg.is_file() {
            args.extend(["--config", cfg_s.as_str()]);
        }
        let _ = run(bin.to_str().unwrap_or("gitleaks"), &args, None);
        if let Ok(t) = std::fs::read_to_string(&tmp) {
            if let Ok(arr) = serde_json::from_str::<Vec<Value>>(&t) {
                for h in arr.iter().take(MAX_ITEMS) {
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
                    .take(MAX_ITEMS)
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
            .take(10)
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
