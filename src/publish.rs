//! GitHub publishing with the house rules baked in: license default, derived-code
//! license match, README requirement, secret scan gate, branch protection.

use crate::tools::repo_of;
use crate::util::{cap_lines, config_dir, epoch, git, out_str, redact, run, which};
use serde_json::{json, Value};

const DEFAULT_LICENSE: &str = "PolyForm-Noncommercial-1.0.0";
const LICENSE_TEXT: &str = include_str!("../assets/PolyForm-Noncommercial-1.0.0.md");
const CI_YML: &str = include_str!("../assets/ci.yml");

pub fn run_publish(a: &Value) -> Result<String, String> {
    let project = a["project"].as_str().ok_or("publish needs project")?;
    let repo_dir = repo_of(project).ok_or(format!("cannot resolve repo for {project}"))?;
    if which("gh").is_none() {
        return Err("gh not on PATH".into());
    }
    let dry = a["dry_run"].as_bool().unwrap_or(true);
    let upstream = a["upstream"].as_str().map(|s| s.trim().to_string());

    // slug: explicit > origin remote > gh user + directory name
    let slug = match a["repo"].as_str() {
        Some(r) => r.to_string(),
        None => {
            let origin = git(&repo_dir, &["remote", "get-url", "origin"]);
            if origin.contains("github.com") {
                origin
                    .trim_end_matches(".git")
                    .rsplit("github.com")
                    .next()
                    .unwrap_or("")
                    .trim_start_matches(['/', ':'])
                    .to_string()
            } else {
                let user = out_str(&run("gh", &["api", "user", "--jq", ".login"], None));
                let name = repo_dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                format!("{}/{}", user.trim(), name)
            }
        }
    };
    let mut blockers: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut actions: Vec<String> = Vec::new();

    // 1. clean tree
    let dirty = git(&repo_dir, &["status", "--porcelain"]);
    if !dirty.trim().is_empty() {
        blockers.push(format!(
            "{} 个文件未提交，先 commit 再发布",
            dirty.lines().count()
        ));
    }

    // 2. README
    match std::fs::read_to_string(repo_dir.join("README.md")) {
        Ok(t) if t.chars().count() >= 600 => {}
        Ok(t) => blockers.push(format!(
            "README.md 只有 {} 字符（要求 ≥600）",
            t.chars().count()
        )),
        Err(_) => blockers.push("缺 README.md".into()),
    }

    // 3. license: derived code must match upstream, otherwise the house default
    let lic_path = repo_dir.join("LICENSE");
    let mut expected = a["license"].as_str().unwrap_or(DEFAULT_LICENSE).to_string();
    let mut upstream_text: Option<String> = None;
    if let Some(up) = &upstream {
        let spdx = out_str(&run(
            "gh",
            &[
                "api",
                &format!("repos/{up}/license"),
                "--jq",
                ".license.spdx_id",
            ],
            None,
        ));
        let spdx = spdx.trim().to_string();
        if spdx.is_empty() || spdx == "null" {
            warnings.push(format!("{up} 未声明 license，无法对齐（保持 {expected}）"));
        } else {
            expected = spdx.clone();
            let body = out_str(&run(
                "gh",
                &["api", &format!("repos/{up}/license"), "--jq", ".content"],
                None,
            ));
            if let Some(t) = decode_b64(&body) {
                upstream_text = Some(t);
            }
        }
    }
    let cur = std::fs::read_to_string(&lic_path).unwrap_or_default();
    if cur.trim().is_empty() {
        let body = upstream_text
            .clone()
            .unwrap_or_else(|| LICENSE_TEXT.to_string());
        actions.push(format!("写 LICENSE（{expected}）"));
        if !dry {
            std::fs::write(&lic_path, body).map_err(|e| e.to_string())?;
        }
    } else if !normalized(&cur).contains(&normalized(&expected)) {
        blockers.push(format!(
            "LICENSE 与期望的 {expected} 不一致（派生自上游就必须同 license）"
        ));
    }

    // 4. secret scan gate
    let tmp = std::env::temp_dir().join(format!("yt-dev-pub-{}.json", epoch()));
    let cfg = config_dir().join("gitleaks.toml");
    if let Some(bin) = which("gitleaks") {
        let mut args: Vec<String> = vec![
            "dir".into(),
            repo_dir.display().to_string(),
            "--report-format".into(),
            "json".into(),
            "--no-banner".into(),
            "--exit-code".into(),
            "0".into(),
            "--max-target-megabytes".into(),
            "5".into(),
            "--report-path".into(),
            tmp.display().to_string(),
        ];
        if cfg.is_file() {
            args.push("--config".into());
            args.push(cfg.display().to_string());
        }
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let _ = run(bin.to_str().unwrap_or("gitleaks"), &refs, None);
        let hits = std::fs::read_to_string(&tmp)
            .ok()
            .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok())
            .unwrap_or_default();
        let _ = std::fs::remove_file(&tmp);
        if !hits.is_empty() {
            blockers.push(format!("gitleaks 命中 {} 条", hits.len()));
            for h in hits.iter().take(5) {
                blockers.push(format!(
                    "  [{}] {}:{} {}",
                    h["RuleID"].as_str().unwrap_or("?"),
                    h["File"].as_str().unwrap_or("?"),
                    h["StartLine"].as_i64().unwrap_or(0),
                    redact(h["Match"].as_str().unwrap_or(""))
                ));
            }
        }
    } else {
        warnings.push("gitleaks 缺失，未做隐私扫描".into());
    }

    // 5. CI workflow (required by the protection rule below)
    let wf = repo_dir.join(".github/workflows/ci.yml");
    if !wf.is_file() {
        actions.push("写 .github/workflows/ci.yml（保护规则要求 ci 通过）".into());
        if !dry {
            std::fs::create_dir_all(wf.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::write(&wf, CI_YML).map_err(|e| e.to_string())?;
        }
    }

    // 6. branch + remote + push
    let branch = git(&repo_dir, &["rev-parse", "--abbrev-ref", "HEAD"]);
    if branch != "main" {
        actions.push(format!("git branch -M main（当前 {branch}）"));
    }
    let has_origin = !git(&repo_dir, &["remote"])
        .lines()
        .all(|l| l.trim() != "origin");
    let exists = run("gh", &["repo", "view", &slug, "--json", "name"], None)
        .status
        .success();
    if !exists {
        actions.push(format!(
            "gh repo create {slug} --public --source . --remote origin --push"
        ));
    } else if !has_origin {
        actions.push(format!("git remote add origin git@github.com:{slug}.git"));
        actions.push("git push -u origin main".into());
    } else if git(
        &repo_dir,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .is_empty()
    {
        actions.push("git push -u origin main".into());
    } else {
        actions.push("git push".into());
    }
    actions.push(format!(
        "保护 main：禁止强推/删除，要求 ci 通过（enforce_admins=true，不强制 PR —— 单人仓库）"
    ));

    let mut out = format!(
        "== publish {project} → {slug} ({})\n",
        if dry { "dry-run" } else { "LIVE" }
    );
    out.push_str(&format!("\nblockers: {}\n", blockers.len()));
    for b in &blockers {
        out.push_str(&format!("  - {b}\n"));
    }
    out.push_str(&format!("warnings: {}\n", warnings.len()));
    for w in &warnings {
        out.push_str(&format!("  - {w}\n"));
    }
    out.push_str("actions:\n");
    for a in &actions {
        out.push_str(&format!("  - {a}\n"));
    }
    if !blockers.is_empty() {
        out.push_str("\n阻断：先清掉 blockers 再发布（license/README/密钥任一不过都不推送）\n");
        return Ok(out);
    }
    if dry {
        out.push_str("\n未执行任何动作（dry_run=true）\n");
        return Ok(out);
    }

    // --- execute
    if branch != "main" {
        git(&repo_dir, &["branch", "-M", "main"]);
    }
    if !exists {
        let vis = a["visibility"].as_str().unwrap_or("public");
        let o = run(
            "gh",
            &[
                "repo",
                "create",
                &slug,
                &format!("--{vis}"),
                "--source",
                ".",
                "--remote",
                "origin",
                "--push",
            ],
            Some(&repo_dir),
        );
        if !o.status.success() {
            return Err(format!(
                "gh repo create 失败: {}",
                cap_lines(&out_str(&o), 8)
            ));
        }
    } else {
        if !has_origin {
            git(
                &repo_dir,
                &[
                    "remote",
                    "add",
                    "origin",
                    &format!("git@github.com:{slug}.git"),
                ],
            );
        }
        let push = run("git", &["push", "-u", "origin", "main"], Some(&repo_dir));
        if !push.status.success() {
            return Err(format!("git push 失败: {}", cap_lines(&out_str(&push), 8)));
        }
    }

    // --- protection + verify
    let payload = json!({
        "required_status_checks": {"strict": true, "contexts": ["ci"]},
        "enforce_admins": true,
        "required_pull_request_reviews": null,
        "restrictions": null,
        "allow_force_pushes": false,
        "allow_deletions": false
    });
    let pf = std::env::temp_dir().join(format!("yt-dev-prot-{}.json", epoch()));
    std::fs::write(&pf, payload.to_string()).map_err(|e| e.to_string())?;
    let prot = run(
        "gh",
        &[
            "api",
            "-X",
            "PUT",
            &format!("repos/{slug}/branches/main/protection"),
            "--input",
            pf.to_str().unwrap_or(""),
        ],
        None,
    );
    let _ = std::fs::remove_file(&pf);
    if !prot.status.success() {
        out.push_str(&format!(
            "\n⚠ 保护规则设置失败: {}\n",
            cap_lines(&out_str(&prot), 6)
        ));
    } else {
        let check = out_str(&run(
            "gh",
            &["api", &format!("repos/{slug}/branches/main/protection"), "--jq",
              "[.required_status_checks.contexts, .allow_force_pushes.enabled, .enforce_admins.enabled]"],
            None,
        ));
        out.push_str(&format!("\n保护规则已生效: {}\n", check.trim()));
    }
    let url = format!("https://github.com/{slug}");
    out.push_str(&format!("推送完成: {url}\n"));
    Ok(out)
}

/// SPDX ids and license titles differ in punctuation; compare letters+digits only.
fn normalized(s: &str) -> String {
    // SPDX ids and license titles differ in punctuation and in the word "license"
    // ("PolyForm-Noncommercial-1.0.0" vs "PolyForm Noncommercial License 1.0.0").
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase()
        .replace("license", "")
}

fn decode_b64(s: &str) -> Option<String> {
    use std::io::Write;
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let mut cmd = std::process::Command::new("base64");
    cmd.arg("-d")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    child.stdin.as_mut()?.write_all(clean.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}
