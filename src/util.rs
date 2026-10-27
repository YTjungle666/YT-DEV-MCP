use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home/ytjungle".into()))
}

pub fn plan_root() -> PathBuf {
    home().join("Plan")
}

pub fn config_dir() -> PathBuf {
    home().join(".config/ytdev")
}

pub fn epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn age_secs(t: SystemTime) -> i64 {
    t.elapsed().map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn run(program: &str, args: &[&str], cwd: Option<&Path>) -> Output {
    let mut c = Command::new(program);
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    c.output().unwrap_or_else(|e| {
        let mut fallback = Command::new("false").spawn().ok();
        if let Some(f) = fallback.as_mut() {
            let _ = f.wait();
        }
        Output {
            status: std::process::ExitStatus::default(),
            stdout: Vec::new(),
            stderr: format!("spawn {program}: {e}").into_bytes(),
        }
    })
}

pub fn out_str(o: &Output) -> String {
    let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
    let e = String::from_utf8_lossy(&o.stderr);
    if !e.trim().is_empty() {
        s.push_str(&e);
    }
    s
}

pub fn which(bin: &str) -> Option<PathBuf> {
    let p = Path::new(bin);
    if p.is_absolute() && p.exists() {
        return Some(p.to_path_buf());
    }
    for d in std::env::var("PATH").unwrap_or_default().split(':') {
        if d.is_empty() {
            continue;
        }
        let c = Path::new(d).join(bin);
        if c.is_file() {
            return Some(c);
        }
    }
    None
}

pub fn selftest() -> i32 {
    let checks = [
        ("gitleaks", "gh release download v8.30.1 -R gitleaks/gitleaks -p 'gitleaks_8.30.1_linux_x64.tar.gz'"),
        ("osv-scanner", "gh release download v2.7.0 -R google/osv-scanner -p osv-scanner_linux_amd64"),
        ("cargo", "rustup/apt: install cargo"),
        ("codex", "npm install -g @openai/codex"),
        ("git", "apt install git"),
    ];
    let mut bad = 0;
    for (b, hint) in checks {
        match which(b) {
            Some(p) => println!("ok       {b} -> {}", p.display()),
            None => {
                println!("MISSING  {b}  ({hint})");
                bad += 1;
            }
        }
    }
    let plans = std::fs::read_dir(plan_root())
        .map(|d| d.filter_map(|e| e.ok()).count())
        .unwrap_or(0);
    println!("ok       ~/Plan entries: {plans}");
    if bad > 0 {
        eprintln!("\n{bad} required binaries missing");
    }
    i32::from(bad > 0)
}

/// Keep report text out of the model's context: secrets never leave verbatim.
pub fn redact(s: &str) -> String {
    let s = s.trim();
    if s.len() <= 4 {
        return "***".into();
    }
    let head: String = s.chars().take(4).collect();
    format!("{head}…({} chars)", s.chars().count())
}

pub fn cap_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= n {
        return s.to_string();
    }
    let tail: Vec<&str> = lines[lines.len() - n..].to_vec();
    format!("…({} lines omitted)\n{}", lines.len() - n, tail.join("\n"))
}

/// Parse the repo path a PLAN.md declares, e.g. ``仓库目标位置：`/home/x/repo` ``.
pub fn repo_from_plan(text: &str) -> Option<String> {
    let re = Regex::new(r"(?:仓库目标位置|交付仓库|仓库位置)\s*[：:]\s*`([^`]+)`").ok()?;
    let raw = re.captures(text)?.get(1)?.as_str().trim().to_string();
    if raw.starts_with('/') {
        return Some(raw);
    }
    // `owner/name` slug -> ~/name, else the bare name.
    let name = raw.rsplit('/').next()?.to_string();
    let cand = home().join(&name);
    Some(cand.to_string_lossy().into_owned())
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let o = run("git", args, Some(dir));
    out_str(&o).trim().to_string()
}
