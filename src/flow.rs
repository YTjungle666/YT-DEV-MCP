//! 编排层：PLAN 项目的规范流程。能力层只认仓库路径，这里负责 project→path、
//! 阶段顺序与断点续跑。状态事实源是 ~/Plan/<项目>/PLAN.md 的勾选，加
//! XDG 里每个阶段的执行记录（actions/<kind>.json，按 HEAD 对齐）。

use crate::plan;
use crate::util::{epoch, git, repo_state_dir};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Plan,
    Execute,
    Review,
    Gate,
    Release,
    Publish,
    Done,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Stage::Plan => "plan",
            Stage::Execute => "execute",
            Stage::Review => "review",
            Stage::Gate => "gate",
            Stage::Release => "release",
            Stage::Publish => "publish",
            Stage::Done => "done",
        }
    }

    fn next(self) -> Stage {
        match self {
            Stage::Plan => Stage::Execute,
            Stage::Execute => Stage::Review,
            Stage::Review => Stage::Gate,
            Stage::Gate => Stage::Release,
            Stage::Release => Stage::Publish,
            Stage::Publish => Stage::Done,
            Stage::Done => Stage::Done,
        }
    }
}

pub fn run_action(a: &Value) -> Result<String, String> {
    match a["action"].as_str().unwrap_or("") {
        "status" => status(a),
        "step" => step(a),
        "advance" => advance(a),
        "init" => init(a),
        other => Err(format!(
            "unknown action {other:?}（init / status / step / advance）"
        )),
    }
}

// ------------------------------------------------------------------ helpers

fn project_of(a: &Value) -> Result<String, String> {
    if let Some(p) = a["project"].as_str() {
        return Ok(p.to_string());
    }
    let path = crate::tools::repo_path(a)?;
    plan::project_for_repo(&path).ok_or_else(|| {
        format!(
            "{} 没有对应的 ~/Plan 项目，先 dev_flow init",
            path.display()
        )
    })
}

/// 项目的仓库：PLAN 声明优先，其次 projects.toml，最后 ~/<项目名>。
fn repo_of_project(project: &str) -> Result<PathBuf, String> {
    crate::tools::repo_of(project).ok_or_else(|| {
        format!("{project} 的仓库没解析出来（PLAN.md 写「仓库目标位置」或在 projects.toml 登记）")
    })
}

fn head(repo: &Path) -> String {
    git(repo, &["rev-parse", "HEAD"])
}

fn actions_dir(repo: &Path) -> PathBuf {
    repo_state_dir(repo).join("actions")
}

/// 记一条阶段执行结果，按 HEAD 对齐（换 commit 即失效，需要重跑）。
fn record(repo: &Path, kind: &str, head: &str, ok: bool, detail: &str) {
    let dir = actions_dir(repo);
    let _ = std::fs::create_dir_all(&dir);
    let v = json!({"kind": kind, "head": head, "ok": ok, "ts": epoch(), "detail": detail});
    let _ = std::fs::write(
        dir.join(format!("{kind}.json")),
        serde_json::to_string_pretty(&v).unwrap_or_default(),
    );
}

fn last(repo: &Path, kind: &str) -> Option<Value> {
    let t = std::fs::read_to_string(actions_dir(repo).join(format!("{kind}.json"))).ok()?;
    serde_json::from_str(&t).ok()
}

/// JSON 是真值且 head 与当前 HEAD 一致。
fn fresh(repo: &Path, kind: &str, head: &str) -> Option<Value> {
    last(repo, kind).filter(|v| v["head"] == json!(head))
}

fn first_open_step(desc: &Value) -> Option<(String, String)> {
    for ph in desc["phases"].as_array()? {
        let title = ph["title"].as_str().unwrap_or("").to_string();
        for it in ph["items"].as_array().cloned().unwrap_or_default() {
            if it["done"] != json!(true) {
                if let Some(text) = it["text"].as_str() {
                    return Some((title, text.to_string()));
                }
            }
        }
    }
    None
}

fn running_workers(repo: &Path) -> Vec<(String, i64)> {
    crate::util::runs_of(repo)
        .into_iter()
        .filter(|(_, m)| crate::util::pid_alive(m["pid"].as_i64().unwrap_or(0)))
        .map(|(id, m)| (id, m["pid"].as_i64().unwrap_or(0)))
        .collect()
}

fn stage_of(project: &str, repo: &Path) -> Result<(Stage, Value), String> {
    let desc = plan::describe(project)?;
    let h = head(repo);
    if desc["status_line"].is_null()
        && desc["phases"]
            .as_array()
            .map(|p| p.is_empty())
            .unwrap_or(true)
    {
        return Ok((Stage::Plan, desc));
    }
    if first_open_step(&desc).is_some() || !running_workers(repo).is_empty() {
        return Ok((Stage::Execute, desc));
    }
    match fresh(repo, "review", &h) {
        Some(r) if r["ok"] == json!(true) => {}
        _ => return Ok((Stage::Review, desc)),
    }
    match fresh(repo, "gate", &h) {
        Some(r) if r["ok"] == json!(true) => {}
        _ => return Ok((Stage::Gate, desc)),
    }
    match fresh(repo, "release", &h) {
        Some(r) if r["ok"] == json!(true) => {}
        _ => return Ok((Stage::Release, desc)),
    }
    match fresh(repo, "publish", &h) {
        Some(r) if r["ok"] == json!(true) => Ok((Stage::Done, desc)),
        _ => Ok((Stage::Publish, desc)),
    }
}

// ------------------------------------------------------------------- status

fn status(a: &Value) -> Result<String, String> {
    let project = project_of(a)?;
    let repo = repo_of_project(&project)?;
    let (stage, desc) = stage_of(&project, &repo)?;
    let h = head(&repo);
    let open = first_open_step(&desc);
    let mut s = format!(
        "project {project} → {}  ({})\n阶段: {}（下个 {}）  · 下一步: {}\n",
        repo.display(),
        h.chars().take(8).collect::<String>(),
        stage.name(),
        stage.next().name(),
        match stage {
            Stage::Plan => "补 PLAN.md（dev_flow init）",
            Stage::Execute => "dev_flow step 起下一个 worker",
            Stage::Review => "dev_flow step 起独立评审",
            Stage::Gate => "dev_flow step 跑门禁（full）",
            Stage::Release => "dev_flow step 做发布预检",
            Stage::Publish => "dev_flow step 预览上传（apply=true 才真推）",
            Stage::Done => "已完成",
        }
    );
    if let Some((title, text)) = open {
        s.push_str(&format!("待办: [{title}] {text}\n"));
    }
    for kind in ["review", "gate", "release", "publish"] {
        if let Some(r) = last(&repo, kind) {
            s.push_str(&format!(
                "{}: {} ({} @ {})\n",
                kind,
                if r["ok"] == json!(true) { "ok" } else { "fail" },
                r["detail"].as_str().unwrap_or(""),
                r["head"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(8)
                    .collect::<String>()
            ));
        }
    }
    let w = running_workers(&repo);
    if w.is_empty() {
        s.push_str("running workers: none\n");
    } else {
        s.push_str(&format!("running workers: {:?}\n", w));
    }
    Ok(s)
}

// --------------------------------------------------------------------- step

fn step(a: &Value) -> Result<String, String> {
    let project = project_of(a)?;
    let repo = repo_of_project(&project)?;
    let (_stage, desc) = stage_of(&project, &repo)?;
    let stage = _stage;
    let h = head(&repo);
    let mut args = json!({"path": repo.display().to_string()});

    let (out, kind, ok) = match stage {
        Stage::Plan => {
            return Err(format!(
                "{project} 的 PLAN.md 还没成形：先 dev_flow init 生成骨架，或 dev_plan 看现状"
            ))
        }
        Stage::Execute => {
            let (title, text) = first_open_step(&desc)
                .ok_or_else(|| "没有待办步骤，但仍有 worker 在跑：dev_worker status".to_string())?;
            if !running_workers(&repo).is_empty() {
                return Ok(format!(
                    "已有 worker 在跑：{:?}\n先 dev_worker action=status 看进度，全部结束后再 step",
                    running_workers(&repo)
                ));
            }
            args["action"] = json!("start");
            args["task"] = json!(text);
            args["scope"] = json!([title]);
            let out = crate::worker::run_action(&args)?;
            return Ok(format!("[{stage}]\n{out}", stage = stage.name()));
        }
        Stage::Review => {
            args["base"] = json!(h);
            let out = crate::tools::review(&args)?;
            let ok = out.contains("verdict: pass") || out.contains("verdict: \"pass\"");
            (out, "review", ok)
        }
        Stage::Gate => {
            args["scope"] = json!("full");
            let out = crate::tools::gate(&args)?;
            let ok = out.contains(" 0 failed");
            (out, "gate", ok)
        }
        Stage::Release => {
            let version = a["version"]
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| version_from_plan(&desc))
                .ok_or("release 阶段需要 version（PLAN 状态行里没找到 vX.Y.Z）")?;
            args["version"] = json!(version);
            let out = crate::tools::release(&args)?;
            let ok = out.contains("blockers: 0");
            (out, "release", ok)
        }
        Stage::Publish => {
            args["dry_run"] = json!(a["apply"] != json!(true));
            let out = crate::publish::run_publish(&args)?;
            let ok = out.contains("blockers: 0") && a["apply"] == json!(true);
            (out, "publish", ok)
        }
        Stage::Done => return Ok("已在 done：没有可推进的步骤".into()),
    };

    record(&repo, kind, &h, ok, out.lines().next().unwrap_or(""));
    Ok(format!(
        "[{}] {}\n{}",
        stage.name(),
        if ok {
            "ok"
        } else {
            "fail（阶段停在原地）"
        },
        out
    ))
}

fn version_from_plan(desc: &Value) -> Option<String> {
    let s = desc["status_line"].as_str()?;
    let mut best = None;
    for tok in s.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        if tok.split('.').count() >= 2 && tok.chars().any(|c| c == '.') {
            best = Some(tok.to_string());
        }
    }
    best
}

fn advance(a: &Value) -> Result<String, String> {
    let mut out = Vec::new();
    let mut args = a.clone();
    for _ in 0..6 {
        let before = status(&args)?;
        let s = step(&args)?;
        out.push(s);
        let after = status(&args)?;
        // 阶段没变且没有新东西可推，就停。
        if stage_line(&before) == stage_line(&after) || after.contains("阶段: done") {
            break;
        }
        if a["once"] == json!(true) {
            break;
        }
        args["version"] = a["version"].clone();
    }
    Ok(out.join("\n---\n"))
}

fn stage_line(s: &str) -> String {
    s.lines()
        .find(|l| l.starts_with("阶段:"))
        .unwrap_or("")
        .to_string()
}

// --------------------------------------------------------------------- init

fn init(a: &Value) -> Result<String, String> {
    let project = a["project"]
        .as_str()
        .ok_or("init needs project（~/Plan 下的目录名）")?
        .to_string();
    let dir = crate::util::plan_root().join(&project);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut done = Vec::new();
    if plan::plan_path(&project).is_none() {
        let path = crate::tools::repo_path(a).ok();
        let repo_line = path
            .as_ref()
            .map(|p| format!("仓库目标位置：`{}`\n", p.display()))
            .unwrap_or_else(|| "仓库目标位置：<待填>\n".to_string());
        let body = format!(
            "# {project}\n\n状态：v0.1（新建，待规划）\n{repo_line}\n---\n\n## 目标\n\n## 非目标\n\n## 硬约束\n\n## 分阶段\n\n### P0 骨架\n- [ ] 第一件能独立验收的活\n\n## 下一步\n- 补计划\n"
        );
        std::fs::write(dir.join("PLAN.md"), body).map_err(|e| e.to_string())?;
        done.push("PLAN.md 骨架".to_string());
    }
    if let Ok(p) = crate::tools::repo_path(a) {
        let f = crate::config::dir().join("projects.toml");
        let mut map = std::fs::read_to_string(&f)
            .ok()
            .and_then(|t| t.parse::<toml::Value>().ok())
            .and_then(|v| v.as_table().cloned())
            .unwrap_or_default();
        map.insert(
            project.clone(),
            toml::Value::String(p.display().to_string()),
        );
        let _ = std::fs::create_dir_all(crate::config::dir());
        let _ = std::fs::write(&f, toml::to_string(&map).unwrap_or_default());
        done.push(format!("projects.toml → {}", p.display()));
    }
    if done.is_empty() {
        Ok(format!("{project} 已存在，没改动"))
    } else {
        Ok(format!("init {project}: {}", done.join("；")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn version_parsed_from_status_line() {
        let d = json!({"status_line": "状态：v0.3.0（已发）"});
        assert_eq!(version_from_plan(&d).as_deref(), Some("0.3.0"));
        assert_eq!(
            version_from_plan(&json!({"status_line": "状态：刚起步"})),
            None
        );
    }

    #[test]
    fn first_open_step_skips_done_items() {
        let d = json!({"phases": [
            {"title": "P0", "items": [{"text": "a", "done": true}, {"text": "b", "done": false}]}
        ]});
        assert_eq!(first_open_step(&d), Some(("P0".into(), "b".into())));
        assert_eq!(first_open_step(&json!({"phases": []})), None);
    }

    #[test]
    fn stage_names_advance_one_by_one() {
        assert_eq!(Stage::Plan.next(), Stage::Execute);
        assert_eq!(Stage::Publish.next(), Stage::Done);
        assert_eq!(Stage::Done.next(), Stage::Done);
    }
}
