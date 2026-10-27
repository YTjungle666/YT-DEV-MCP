use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

pub fn plan_root() -> PathBuf {
    home().join("Plan")
}

pub fn config_dir() -> PathBuf {
    home().join(".config/yt-dev")
}

pub fn epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Unique run id: seconds plus sub-second nanos (two workers can start in the
/// same second and would otherwise collide on branch names).
pub fn stamp() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() % 1_000_000)
        .unwrap_or(0);
    format!("{}-{nanos:06}", epoch())
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
        ("gh", "apt install gh"),
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

// ------------------------------------------------------------ workflows

pub fn runs_dir(project: &str) -> PathBuf {
    plan_root().join(project).join("runs")
}

/// All run records, newest first.
pub fn all_runs() -> Vec<(String, serde_json::Value)> {
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(plan_root()) else {
        return out;
    };
    for d in dirs.filter_map(|e| e.ok()) {
        let rd = d.path().join("runs");
        let Ok(files) = std::fs::read_dir(&rd) else {
            continue;
        };
        for f in files.filter_map(|e| e.ok()) {
            let path = f.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            if let Ok(t) = std::fs::read_to_string(&path) {
                if let Ok(v) = serde_json::from_str(&t) {
                    let id = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    out.push((id, v));
                }
            }
        }
    }
    out.sort_by_key(|(_, v)| -v["started"].as_i64().unwrap_or(0));
    out
}

pub fn pid_alive(pid: i64) -> bool {
    pid > 0 && Path::new(&format!("/proc/{pid}")).exists()
}

pub fn cap1(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

/// CPU% and RSS(MB) of a pid, read from ps so no /proc parsing is needed.
pub fn proc_stats(pid: i64) -> (f64, u64) {
    let o = run("ps", &["-o", "%cpu=,rss=", "-p", &pid.to_string()], None);
    let s = out_str(&o);
    let mut it = s.split_whitespace();
    let cpu = it.next().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    let rss_kb = it.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    (cpu, rss_kb / 1024)
}

/// Free bytes on the filesystem holding `p`.
pub fn free_bytes(p: &Path) -> u64 {
    let o = run("df", &["-P", "-B1", p.to_str().unwrap_or(".")], None);
    out_str(&o)
        .lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().nth(3))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}
