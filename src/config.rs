//! 所有可调项集中在 $YT_DEV_CONFIG_DIR/config.toml（默认
//! ~/.config/yt-dev/config.toml）。缺键取代码里的 Default，未知键忽略，
//! 路径里的 `~/` 会展开；文件改了下一次工具调用就生效，不用重启 MCP。
//! `yt-dev-mcp --init-config` 会把带注释的完整例子写出来。

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

pub const EXAMPLE: &str = include_str!("../assets/config.example.toml");

pub fn home() -> PathBuf {
    crate::util::home()
}

/// 配置目录：`$YT_DEV_CONFIG_DIR`，否则 `~/.config/yt-dev`。
pub fn dir() -> PathBuf {
    std::env::var_os("YT_DEV_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config/yt-dev"))
}

pub fn file() -> PathBuf {
    dir().join("config.toml")
}

fn expand(p: &Path) -> PathBuf {
    match p.to_string_lossy().strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => p.to_path_buf(),
    }
}

// ------------------------------------------------------------------- schema

#[derive(Deserialize, Clone, Default)]
#[serde(default)]
pub struct Config {
    pub paths: Paths,
    pub limits: Limits,
    pub plan: PlanCfg,
    pub gate: Gate,
    pub worker: Worker,
    pub review: Review,
    pub publish: Publish,
    pub selftest: Selftest,
}

/// 计划、状态、日志放在哪儿。空的可选文件表示“不存在就用内置默认”。
#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Paths {
    pub plan_root: PathBuf,
    pub state_dir: PathBuf,
    pub projects: PathBuf,
    pub worker_prompt: PathBuf,
    pub review_prompt: PathBuf,
    pub gitleaks_config: PathBuf,
    pub license_file: PathBuf,
    pub ci_template: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        let d = dir();
        Self {
            plan_root: home().join("Plan"),
            state_dir: home().join(".cache/yt-dev"),
            projects: d.join("projects.toml"),
            worker_prompt: d.join("worker-prompt.md"),
            review_prompt: d.join("review-prompt.md"),
            gitleaks_config: d.join("gitleaks.toml"),
            license_file: d.join("LICENSE"),
            ci_template: d.join("ci.yml"),
        }
    }
}

impl Paths {
    fn expand(&mut self) {
        for p in [
            &mut self.plan_root,
            &mut self.state_dir,
            &mut self.projects,
            &mut self.worker_prompt,
            &mut self.review_prompt,
            &mut self.gitleaks_config,
            &mut self.license_file,
            &mut self.ci_template,
        ] {
            *p = expand(p);
        }
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Limits {
    /// run 日志/元数据 + worktree 的总占用上限（MB）。
    pub max_store_mb: u64,
    /// 无论占用多少，最多保留这么多个 run。
    pub keep_runs: usize,
    pub mem_mb: u64,
    pub cpu_pct: u32,
    pub max_parallel: usize,
    pub gate_timeout_secs: u64,
    pub slice_max_chars: usize,
    pub max_items: usize,
    pub readme_min_chars: usize,
    pub stall_secs: i64,
    pub log_tail_lines: usize,
    pub gitleaks_max_mb: u64,
    pub decisions_max: usize,
    pub constraint_lines_max: usize,
    pub kill_grace_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_store_mb: 4096,
            keep_runs: 30,
            mem_mb: 4096,
            cpu_pct: 50,
            max_parallel: 6,
            gate_timeout_secs: 600,
            slice_max_chars: 4000,
            max_items: 20,
            readme_min_chars: 600,
            stall_secs: 300,
            log_tail_lines: 30,
            gitleaks_max_mb: 5,
            decisions_max: 20,
            constraint_lines_max: 10,
            kill_grace_secs: 10,
        }
    }
}

/// PLAN.md 的词汇表：换一种写法的计划只改这里，不用改代码。
#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct PlanCfg {
    pub files: Vec<String>,
    pub phase_sections: Vec<String>,
    pub verify_section: String,
    pub todo_sections: Vec<String>,
    pub next_section: String,
    pub constraint_sections: Vec<String>,
    pub status_markers: Vec<String>,
    pub decision_prefix: String,
    pub repo_markers: Vec<String>,
}

impl Default for PlanCfg {
    fn default() -> Self {
        Self {
            files: ["PLAN.md", "plan.md", "Plan.md"].map(String::from).to_vec(),
            phase_sections: ["分阶段", "实施步骤", "步骤"].map(String::from).to_vec(),
            verify_section: "验证".into(),
            todo_sections: ["YT 侧待办", "YT 待办"].map(String::from).to_vec(),
            next_section: "下一步".into(),
            constraint_sections: ["硬约束", "非目标", "边界"].map(String::from).to_vec(),
            status_markers: ["状态", "版本"].map(String::from).to_vec(),
            decision_prefix: "D".into(),
            repo_markers: ["仓库目标位置", "交付仓库", "仓库位置"]
                .map(String::from)
                .to_vec(),
        }
    }
}

/// 门禁：按顺序找第一个命中 `detect` 文件的 profile；`[gate.overrides.<project>]`
/// 优先于 profile。加一种语言只是加一个 profile，不用改代码。
#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Gate {
    pub profiles: Vec<GateProfile>,
    pub overrides: BTreeMap<String, Vec<GateStep>>,
}

#[derive(Deserialize, Clone, Default)]
#[serde(default)]
pub struct GateProfile {
    pub name: String,
    pub detect: Vec<String>,
    pub steps: Vec<GateStep>,
}

#[derive(Deserialize, Clone, Default)]
#[serde(default)]
pub struct GateStep {
    /// 报告里的步骤名；省略时用 cmd 的第一个词。
    pub name: String,
    pub cmd: Vec<String>,
    /// 只在 dev_gate scope=full 时跑。
    pub full_only: bool,
    /// 命中文件的内容必须包含这段字样才跑（如 package.json 里有没有 "test"）。
    pub needs: Option<String>,
}

impl Default for Gate {
    fn default() -> Self {
        let step = |name: &str, cmd: &[&str]| GateStep {
            name: name.into(),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            ..GateStep::default()
        };
        let needed = |name: &str, cmd: &[&str], needs: &str| GateStep {
            needs: Some(needs.into()),
            ..step(name, cmd)
        };
        Self {
            profiles: vec![
                GateProfile {
                    name: "rust".into(),
                    detect: vec!["Cargo.toml".into()],
                    steps: vec![
                        step("fmt", &["cargo", "fmt", "--check"]),
                        step(
                            "clippy",
                            &["cargo", "clippy", "--all-targets", "--", "-D", "warnings"],
                        ),
                        step("test", &["cargo", "test"]),
                        GateStep {
                            full_only: true,
                            ..step("build-release", &["cargo", "build", "--release"])
                        },
                    ],
                },
                GateProfile {
                    name: "node".into(),
                    detect: vec!["package.json".into()],
                    steps: vec![
                        needed("build", &["npm", "run", "build"], "\"build\""),
                        needed("test", &["npm", "test"], "\"test\""),
                    ],
                },
                GateProfile {
                    name: "make".into(),
                    detect: vec!["Makefile".into()],
                    steps: vec![needed("verify", &["make", "verify"], "verify:")],
                },
            ],
            overrides: BTreeMap::new(),
        }
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Worker {
    pub bin: String,
    /// prompt 追加在这些参数之后。
    pub args: Vec<String>,
    pub nice: i32,
    pub branch_prefix: String,
    pub worktree_subdir: String,
    /// 模板正文；`paths.worker_prompt` 文件存在时以文件为准。
    pub prompt: String,
    /// 硬约束条目，按顺序编号；条目里的 `{…}` 占位符也会被替换。
    pub rules: Vec<String>,
    /// `{max_file_lines}` 的取值（模块化条款）。
    pub max_file_lines: usize,
    /// `{max_fn_lines}` 的取值。
    pub max_fn_lines: usize,
}

impl Default for Worker {
    fn default() -> Self {
        Self {
            bin: "codex".into(),
            args: ["exec", "--dangerously-bypass-approvals-and-sandbox"]
                .map(String::from)
                .to_vec(),
            nice: 10,
            branch_prefix: "yt-dev/".into(),
            worktree_subdir: "wt".into(),
            prompt: crate::worker::DEFAULT_PROMPT.into(),
            rules: crate::worker::DEFAULT_RULES
                .iter()
                .map(|r| r.to_string())
                .collect(),
            max_file_lines: 400,
            max_fn_lines: 60,
        }
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Review {
    pub bin: String,
    /// `{out}` 会被换成结果文件的绝对路径，prompt 追加在最后。
    pub args: Vec<String>,
    /// 模板正文；`paths.review_prompt` 文件存在时以文件为准。
    pub prompt: String,
}

impl Default for Review {
    fn default() -> Self {
        Self {
            bin: "codex".into(),
            args: [
                "exec",
                "--profile",
                "strong",
                "--dangerously-bypass-approvals-and-sandbox",
                "-o",
                "{out}",
            ]
            .map(String::from)
            .to_vec(),
            prompt: crate::tools::DEFAULT_REVIEW_PROMPT.into(),
        }
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Publish {
    pub license: String,
    pub branch: String,
    pub ci_file: String,
    pub remote_template: String,
    pub required_checks: Vec<String>,
    pub enforce_admins: bool,
    pub visibility: String,
}

impl Default for Publish {
    fn default() -> Self {
        Self {
            license: "PolyForm-Noncommercial-1.0.0".into(),
            branch: "main".into(),
            ci_file: ".github/workflows/ci.yml".into(),
            remote_template: "git@github.com:{slug}.git".into(),
            required_checks: vec!["ci".into()],
            enforce_admins: false,
            visibility: "public".into(),
        }
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Selftest {
    pub required: Vec<BinReq>,
}

#[derive(Deserialize, Clone, Default)]
#[serde(default)]
pub struct BinReq {
    pub name: String,
    pub hint: String,
}

impl Default for Selftest {
    fn default() -> Self {
        let req = |name: &str, hint: &str| BinReq {
            name: name.into(),
            hint: hint.into(),
        };
        Self {
            required: vec![
                req("gitleaks", "gh release download -R gitleaks/gitleaks"),
                req("osv-scanner", "gh release download -R google/osv-scanner"),
                req("cargo", "rustup/apt: install cargo"),
                req("codex", "npm install -g @openai/codex"),
                req("git", "apt install git"),
                req("gh", "apt install gh"),
            ],
        }
    }
}

// --------------------------------------------------------------------- load

pub fn load() -> Config {
    let f = file();
    let mut c = match std::fs::read_to_string(&f) {
        Ok(t) => match toml::from_str::<Config>(&t) {
            Ok(c) => c,
            Err(e) => {
                // 静默退回默认值会藏住打错键名，至少让 stderr 里有痕迹。
                eprintln!("yt-dev: {} 解析失败，本次用代码默认值: {e}", f.display());
                Config::default()
            }
        },
        Err(_) => Config::default(),
    };
    c.paths.expand();
    c
}

static CFG: RwLock<Option<(&'static Config, Option<SystemTime>)>> = RwLock::new(None);

/// 每次访问 stat 一下配置文件，改了立即生效；解析失败沿用上一次成功的值。
pub fn cfg() -> &'static Config {
    let mtime = std::fs::metadata(file()).and_then(|m| m.modified()).ok();
    if let Some((c, t)) = *CFG.read().unwrap() {
        if t == mtime {
            return c;
        }
    }
    let mut w = CFG.write().unwrap();
    if let Some((c, t)) = *w {
        if t == mtime {
            return c;
        }
    }
    let c: &'static Config = Box::leak(Box::new(load()));
    *w = Some((c, mtime));
    c
}

#[cfg(test)]
pub fn reset() {
    *CFG.write().unwrap() = None;
}

/// `--init-config`：不覆盖已有文件。
pub fn init() -> i32 {
    let f = file();
    if f.exists() {
        println!("exists: {}（未覆盖）", f.display());
        return 0;
    }
    if let Some(p) = f.parent() {
        if let Err(e) = std::fs::create_dir_all(p) {
            eprintln!("mkdir {}: {e}", p.display());
            return 1;
        }
    }
    match std::fs::write(&f, EXAMPLE) {
        Ok(_) => {
            println!("written: {}", f.display());
            0
        }
        Err(e) => {
            eprintln!("write {}: {e}", f.display());
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_toml_keeps_defaults_for_missing_keys() {
        let c: Config = toml::from_str(
            r#"
[limits]
max_store_mb = 8
"#,
        )
        .unwrap();
        assert_eq!(c.limits.max_store_mb, 8);
        assert_eq!(c.limits.mem_mb, Limits::default().mem_mb);
        assert_eq!(c.publish.branch, "main");
        assert!(!c.gate.profiles.is_empty());
    }

    #[test]
    fn gate_step_overrides_and_flags_parse() {
        let c: Config = toml::from_str(
            r#"
[[gate.profiles]]
name = "go"
detect = ["go.mod"]
steps = [
  { cmd = ["go", "vet", "./..."] },
  { name = "test", cmd = ["go", "test", "./..."], full_only = true, needs = "module" },
]

[gate.overrides]
demo = [{ name = "lint", cmd = ["make", "lint"] }]
"#,
        )
        .unwrap();
        assert_eq!(c.gate.profiles.len(), 1);
        let s = &c.gate.profiles[0].steps;
        assert!(s[0].name.is_empty());
        assert_eq!(s[1].needs.as_deref(), Some("module"));
        assert!(s[1].full_only);
        assert_eq!(c.gate.overrides["demo"][0].cmd, vec!["make", "lint"]);
    }

    #[test]
    fn example_config_parses_and_stays_in_sync() {
        let c: Config = toml::from_str(EXAMPLE).expect("assets/config.example.toml 必须能解析");
        let d = Config::default();
        assert_eq!(c.limits.max_store_mb, d.limits.max_store_mb);
        assert_eq!(c.gate.profiles.len(), d.gate.profiles.len());
        assert_eq!(c.publish.license, d.publish.license);
    }
}
