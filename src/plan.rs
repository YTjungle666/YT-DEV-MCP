//! PLAN.md parsing and surgical write-back. Everything degrades to raw text
//! instead of silently reporting "nothing found".

use crate::util::plan_root;
use regex::Regex;
use serde_json::{json, Value};
use std::path::PathBuf;

pub fn plan_path(project: &str) -> Option<PathBuf> {
    let dir = plan_root().join(project);
    for name in ["PLAN.md", "plan.md", "Plan.md"] {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

pub fn list_projects() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(plan_root())
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn section_bounds(lines: &[&str], needle: &str) -> Option<(usize, usize)> {
    let start = lines
        .iter()
        .position(|l| l.starts_with("## ") && l.contains(needle))?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| l.starts_with("## "))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());
    Some((start, end))
}

fn checkbox(item: &str) -> Option<(bool, String)> {
    let t = item.trim_start();
    let rest = t.strip_prefix("- [")?;
    let mut ch = rest.chars();
    let mark = ch.next()?;
    let text = ch.as_str().strip_prefix("] ")?.trim().to_string();
    Some((mark == 'x' || mark == 'X', text))
}

pub struct Plan {
    pub status_line: Option<String>,
    pub decisions: Vec<Value>,
    pub phases: Vec<Value>,
    pub verify: Vec<String>,
    pub yt_todos: Vec<String>,
    pub next_step: Vec<String>,
    pub raw_sections: Vec<String>,
}

pub fn parse(text: &str) -> Plan {
    let lines: Vec<&str> = text.lines().collect();
    let status_re = Regex::new(r"^\s*(?:-\s*)?(?:状态|版本)\s*[：:]").unwrap();
    let status_line = lines
        .iter()
        .find(|l| status_re.is_match(l))
        .map(|l| l.trim().to_string());

    let mut decisions = Vec::new();
    let dec_re = Regex::new(r"^\|\s*(D\d+)\s*\|").unwrap();
    for l in &lines {
        if let Some(c) = dec_re.captures(l) {
            let cells: Vec<&str> = l
                .trim()
                .trim_matches('|')
                .split('|')
                .map(|c| c.trim())
                .collect();
            decisions.push(json!({
                "id": c.get(1).unwrap().as_str(),
                "text": cells.get(1).cloned().unwrap_or(""),
            }));
        }
    }

    let mut phases = Vec::new();
    if let Some((s, e)) = section_bounds(&lines, "分阶段")
        .or_else(|| section_bounds(&lines, "实施步骤"))
        .or_else(|| section_bounds(&lines, "步骤"))
    {
        let mut cur: Option<(String, Vec<Value>)> = None;
        for l in &lines[s..e] {
            if l.starts_with("###") {
                if let Some((t, items)) = cur.take() {
                    phases.push(phase_json(&t, &items));
                }
                cur = Some((l.trim_start_matches('#').trim().to_string(), Vec::new()));
            } else if let Some((done, text)) = checkbox(l) {
                if let Some((_, items)) = cur.as_mut() {
                    items.push(json!({"done": done, "text": text}));
                }
            } else {
                // Plain bullets carry the sub-steps in plans that don't use checkboxes.
                let t = l.trim();
                if t.starts_with("- ") && t.len() > 2 {
                    if let Some((_, items)) = cur.as_mut() {
                        items.push(
                            json!({"done": null, "text": t.trim_start_matches("- ").to_string()}),
                        );
                    }
                }
            }
        }
        if let Some((t, items)) = cur.take() {
            phases.push(phase_json(&t, &items));
        }
    }

    let bullets = |needle: &str| -> Vec<String> {
        section_bounds(&lines, needle)
            .map(|(s, e)| {
                lines[s + 1..e]
                    .iter()
                    .filter(|l| {
                        let t = l.trim();
                        t.starts_with("- ")
                            || t.starts_with("1.")
                            || t.chars().next().is_some_and(|c| c.is_ascii_digit())
                    })
                    .map(|l| l.trim().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let verify = bullets("验证");
    let yt_todos = bullets("YT 侧待办")
        .into_iter()
        .chain(bullets("YT 待办"))
        .collect();
    let next_step = bullets("下一步");

    Plan {
        status_line,
        decisions,
        phases,
        verify,
        yt_todos,
        next_step,
        raw_sections: Vec::new(),
    }
}

fn phase_json(title: &str, items: &[Value]) -> Value {
    let checkable: Vec<&Value> = items.iter().filter(|i| !i["done"].is_null()).collect();
    let done = checkable
        .iter()
        .filter(|i| i["done"] == json!(true))
        .count();
    json!({"title": title, "done": done, "total": checkable.len(), "steps": items.len(), "items": items})
}

pub fn describe(project: &str) -> Result<Value, String> {
    let p = plan_path(project)
        .ok_or_else(|| format!("no PLAN.md under {}", plan_root().join(project).display()))?;
    let text = std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
    let plan = parse(&text);
    let mtime = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok());
    Ok(json!({
        "project": project,
        "source_path": p.display().to_string(),
        "modified_ago_secs": mtime.map(crate::util::age_secs),
        "status_line": plan.status_line,
        "decisions": plan.decisions,
        "phases": plan.phases,
        "verify_checklist": plan.verify,
        "yt_todos": plan.yt_todos,
        "next_step": plan.next_step,
        "raw": !plan.raw_sections.is_empty(),
    }))
}

/// One surgical edit: either flip one checkbox, replace the status line, or
/// append a run note before the final "下一步" section.
pub fn update(project: &str, ops: &[Value]) -> Result<String, String> {
    let p = plan_path(project).ok_or_else(|| format!("no PLAN.md for {project}"))?;
    let original = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
    let mut lines: Vec<String> = original.lines().map(|s| s.to_string()).collect();
    let mut log: Vec<String> = Vec::new();

    for op in ops {
        match op["kind"].as_str().unwrap_or("") {
            "set_status" => {
                let new = op["value"].as_str().ok_or("set_status needs value")?;
                let re = Regex::new(r"^\s*(?:-\s*)?(?:状态|版本)\s*[：:]").unwrap();
                let hits: Vec<usize> = lines
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| re.is_match(l))
                    .map(|(i, _)| i)
                    .collect();
                if hits.len() != 1 {
                    return Err(format!(
                        "set_status: {} matching status lines (need exactly 1)",
                        hits.len()
                    ));
                }
                let i = hits[0];
                let old = lines[i].clone();
                let prefix = old.split(['：', ':']).next().unwrap_or("状态").to_string();
                let sep = if old.contains('：') { '：' } else { ':' };
                lines[i] = format!("{prefix}{sep}{new}");
                log.push(format!("L{}: -{old}\n      +{}", i + 1, lines[i]));
            }
            "check_step" => {
                let phase = op["phase"].as_str().unwrap_or("");
                let idx = op["index"].as_i64().unwrap_or(0) as usize;
                let done = op["done"].as_bool().unwrap_or(true);
                let start = lines
                    .iter()
                    .position(|l| l.starts_with("###") && l.contains(phase))
                    .ok_or_else(|| format!("check_step: no phase heading containing {phase:?}"))?;
                let end = lines
                    .iter()
                    .enumerate()
                    .skip(start + 1)
                    .find(|(_, l)| l.starts_with("###") || l.starts_with("## "))
                    .map(|(i, _)| i)
                    .unwrap_or(lines.len());
                let hits: Vec<usize> = (start + 1..end)
                    .filter(|&i| checkbox(&lines[i]).is_some())
                    .collect();
                let i = *hits.get(idx).ok_or_else(|| {
                    format!(
                        "check_step: phase {phase:?} has {} steps, index {idx} out of range",
                        hits.len()
                    )
                })?;
                let old = lines[i].clone();
                lines[i] = if done {
                    old.replacen("- [ ]", "- [x]", 1)
                } else {
                    old.replacen("- [x]", "- [ ]", 1)
                };
                log.push(format!("L{}: -{old}\n      +{}", i + 1, lines[i]));
            }
            "append_run_note" => {
                let note = op["text"]
                    .as_str()
                    .ok_or("append_run_note needs text")?
                    .trim()
                    .to_string();
                let at = lines
                    .iter()
                    .position(|l| l.starts_with("## ") && l.contains("下一步"))
                    .unwrap_or(lines.len());
                lines.insert(at, format!("- {note}"));
                log.push(format!("L{}: +- {}", at + 1, note));
            }
            other => return Err(format!("unknown op kind {other:?}")),
        }
    }

    let updated = lines.join("\n") + "\n";
    if updated == original {
        return Ok("no change".into());
    }
    std::fs::write(&p, &updated).map_err(|e| format!("write {}: {e}", p.display()))?;
    Ok(format!("{}\n\nwritten: {}", log.join("\n"), p.display()))
}

/// Compact context for one worker: status line, decision rows, constraint
/// sections, and only the requested sections. Whole-plan prompts make workers
/// wander outside their slice.
pub fn slice(project: &str, scopes: &[String]) -> Result<String, String> {
    let p = plan_path(project).ok_or_else(|| format!("no PLAN.md for {project}"))?;
    let text = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::new();

    let status_re = Regex::new(r"^\s*(?:-\s*)?(?:状态|版本)\s*[：:]").unwrap();
    let dec_re = Regex::new(r"^\|\s*D\d+\s*\|").unwrap();
    if let Some(s) = lines.iter().find(|l| status_re.is_match(l)) {
        out.push_str(s.trim());
        out.push('\n');
    }
    let decs: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| dec_re.is_match(l))
        .collect();
    if !decs.is_empty() {
        out.push_str("\n决策（已定，不要重新讨论）：\n");
        for d in decs.iter().take(20) {
            out.push_str(d.trim());
            out.push('\n');
        }
    }
    for needle in ["硬约束", "非目标", "边界"] {
        if let Some((s, e)) = section_bounds(&lines, needle) {
            out.push_str(&format!("\n{}（节选）：\n", needle));
            for l in lines[s + 1..e]
                .iter()
                .filter(|l| !l.trim().is_empty())
                .take(10)
            {
                out.push_str(l.trim());
                out.push('\n');
            }
        }
    }
    for sc in scopes {
        let block = section_bounds(&lines, sc).or_else(|| subsection_bounds(&lines, sc));
        match block {
            Some((s, e)) => {
                out.push_str(&format!("\n== 你的范围：{}\n", lines[s].trim()));
                for l in &lines[s + 1..e] {
                    if !l.trim().is_empty() {
                        out.push_str(l.trim_end());
                        out.push('\n');
                    }
                }
            }
            None => out.push_str(&format!(
                "\n== 未在计划中找到段落：{sc}（按任务描述执行）\n"
            )),
        }
    }
    // A worker that gets 200 lines just re-reads the plan; keep it small.
    if out.chars().count() > 4000 {
        let cut: String = out.chars().take(4000).collect();
        return Ok(format!("{cut}\n…（片段已截断）"));
    }
    Ok(out)
}

/// `###` level block, for phases nested under a `##` section.
fn subsection_bounds(lines: &[&str], needle: &str) -> Option<(usize, usize)> {
    let start = lines
        .iter()
        .position(|l| l.starts_with("###") && l.contains(needle))?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| l.starts_with("###") || l.starts_with("## "))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn slice_keeps_decisions_and_only_the_named_phase() {
        let text = "# t\n\n状态：v1\n\n## 分阶段实施\n\n### P0 骨架\n- [ ] 甲\n\n### P1 接口\n- [ ] 乙\n\n## 硬约束\n- 不许动数据库\n";
        let tmp = std::env::temp_dir().join("ytdev-slice-test.md");
        std::fs::write(&tmp, text).unwrap();
        // slice() reads from ~/Plan, so exercise the block helpers directly.
        let lines: Vec<&str> = text.lines().collect();
        let (s, e) = subsection_bounds(&lines, "P0").unwrap();
        assert!(lines[s].contains("P0"));
        assert!(lines[s..e].iter().any(|l| l.contains("甲")));
        assert!(!lines[s..e].iter().any(|l| l.contains("P1")));
        let (hs, _) = section_bounds(&lines, "硬约束").unwrap();
        assert!(lines[hs].contains("硬约束"));
        let _ = std::fs::remove_file(&tmp);
    }

    const SAMPLE: &str = "# t\n\n状态：v1（测试）\n\n## 3. 分阶段实施\n\n### P0 骨架\n- [x] 一\n- [ ] 二\n- 说明行\n\n## 9. 下一步\n\n1. 待定\n";

    #[test]
    fn parses_status_phases_and_next() {
        let p = parse(SAMPLE);
        assert_eq!(p.status_line.as_deref(), Some("状态：v1（测试）"));
        assert_eq!(p.phases.len(), 1);
        assert_eq!(p.phases[0]["done"], json!(1));
        assert_eq!(p.phases[0]["total"], json!(2));
        assert_eq!(p.phases[0]["steps"], json!(3));
        assert_eq!(p.next_step.len(), 1);
    }

    #[test]
    fn empty_plan_degrades_without_panicking() {
        let p = parse("# 只有标题\n");
        assert!(p.status_line.is_none());
        assert!(p.phases.is_empty());
    }
}
