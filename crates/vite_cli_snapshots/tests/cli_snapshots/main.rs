//! PTY-based snapshot test harness for the vp CLI.
//!
//! Fixtures live in `tests/cli_snapshots/fixtures/<name>/`; each declares
//! cases in `snapshots.toml` (see `rfcs/interactive-snapshot-tests.md`).
//! Every step runs in a real pseudo-terminal backed by a vt100 emulator;
//! interactive steps synchronize on OSC 8 milestones emitted by the child.
//! Snapshots are Markdown files compared with real pass/fail semantics
//! (`UPDATE_SNAPSHOTS=1` accepts changes).
//!
//! The harness deliberately uses std types: it is a dev-only test binary,
//! matching the conventions of vite-task's `e2e_snapshots` harness it is
//! ported from.
#![expect(clippy::disallowed_types, reason = "standalone test harness uses std types")]
#![expect(clippy::disallowed_macros, reason = "standalone test harness uses std macros")]
#![expect(clippy::disallowed_methods, reason = "standalone test harness uses std methods")]

mod flavor;
mod redact;

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use cp_r::CopyOptions;
use flavor::{Flavor, FlavorRuntime};
use pty_terminal_test::{CommandBuilder, ScreenSize, TestTerminal};
use redact::redact_output;

/// Default per-step timeout. Windows CI needs longer due to process startup
/// overhead and slower I/O. Individual steps override via `timeout` (ms).
const STEP_TIMEOUT: Duration =
    if cfg!(windows) { Duration::from_secs(60) } else { Duration::from_secs(50) };

/// Screen size for the PTY terminal. Large enough to avoid line wrapping.
const SCREEN_SIZE: ScreenSize = ScreenSize { rows: 500, cols: 500 };

#[derive(serde::Deserialize, Debug)]
#[serde(untagged)]
enum Step {
    /// Shorthand: `["vp", "check"]`
    Simple(Vec<String>),
    /// Detailed: `{ argv = ["vp", "create"], interactions = [...], ... }`
    Detailed(StepConfig),
}

fn default_true() -> bool {
    true
}

#[derive(serde::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct StepConfig {
    argv: Vec<String>,
    /// Optional working directory for this step, relative to the staged
    /// fixture root. Defaults to the case-level `cwd`.
    #[serde(default)]
    cwd: Option<String>,
    /// Rendered under the step heading in the snapshot.
    #[serde(default)]
    comment: Option<String>,
    /// Extra environment variables set for this step.
    #[serde(default)]
    envs: Vec<(String, String)>,
    #[serde(default)]
    interactions: Vec<Interaction>,
    /// When true, render the terminal snapshot with inline ANSI escape codes
    /// (made visible as `\x1b[…m`) so colour/style attributes are part of the
    /// assertion. Default `false` keeps the plain-text behaviour.
    #[serde(default, rename = "formatted-snapshot")]
    formatted_snapshot: bool,
    /// Per-step timeout override in milliseconds.
    #[serde(default)]
    timeout: Option<u64>,
    /// When false, the step still runs (heading and exit code appear in the
    /// snapshot) but its screen is omitted while the step succeeds; failures
    /// always keep their output. Replaces the old `ignoreOutput`, which had
    /// the same on-success-only semantics.
    #[serde(default = "default_true")]
    snapshot: bool,
    /// When false, the step runs with piped stdio instead of a PTY, for cases
    /// that specifically assert non-TTY behaviour. Interactions require a PTY.
    #[serde(default = "default_true")]
    tty: bool,
    /// By default a failing step stops the case (shell-like `&&` semantics);
    /// set true when later steps deliberately inspect post-failure state.
    #[serde(default, rename = "continue-on-failure")]
    continue_on_failure: bool,
}

impl Step {
    fn argv(&self) -> &[String] {
        match self {
            Self::Simple(argv) => argv,
            Self::Detailed(config) => &config.argv,
        }
    }

    /// Shell-escaped command line including any env-var prefix and non-default
    /// cwd, without the comment (e.g. `cd packages/a && MY_ENV=1 vp check`).
    fn display_command_line(&self, default_cwd: &str) -> String {
        let argv_str = self
            .argv()
            .iter()
            .map(|s| {
                if s.contains(|c: char| c.is_whitespace() || c == '"') {
                    shell_escape::escape(s.as_str().into()).into_owned()
                } else {
                    s.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        let command = match self {
            Self::Simple(_) => argv_str,
            Self::Detailed(config) => {
                let mut parts = String::new();
                for (k, v) in &config.envs {
                    parts.push_str(&format!("{k}={v} "));
                }
                parts.push_str(&argv_str);
                parts
            }
        };

        let cwd = self.cwd().unwrap_or(default_cwd);
        if cwd == default_cwd {
            command
        } else {
            let cwd = if cwd.is_empty() { "." } else { cwd };
            format!("cd {} && {command}", shell_escape::escape(cwd.into()))
        }
    }

    fn comment(&self) -> Option<&str> {
        match self {
            Self::Detailed(config) => config.comment.as_deref(),
            Self::Simple(_) => None,
        }
    }

    fn interactions(&self) -> &[Interaction] {
        match self {
            Self::Detailed(config) => &config.interactions,
            Self::Simple(_) => &[],
        }
    }

    fn envs(&self) -> &[(String, String)] {
        match self {
            Self::Detailed(config) => &config.envs,
            Self::Simple(_) => &[],
        }
    }

    fn cwd(&self) -> Option<&str> {
        match self {
            Self::Detailed(config) => config.cwd.as_deref(),
            Self::Simple(_) => None,
        }
    }

    const fn formatted_snapshot(&self) -> bool {
        match self {
            Self::Detailed(config) => config.formatted_snapshot,
            Self::Simple(_) => false,
        }
    }

    fn timeout(&self) -> Duration {
        match self {
            Self::Detailed(StepConfig { timeout: Some(ms), .. }) => Duration::from_millis(*ms),
            _ => STEP_TIMEOUT,
        }
    }

    const fn snapshot(&self) -> bool {
        match self {
            Self::Detailed(config) => config.snapshot,
            Self::Simple(_) => true,
        }
    }

    const fn tty(&self) -> bool {
        match self {
            Self::Detailed(config) => config.tty,
            Self::Simple(_) => true,
        }
    }

    const fn continue_on_failure(&self) -> bool {
        match self {
            Self::Detailed(config) => config.continue_on_failure,
            Self::Simple(_) => false,
        }
    }
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(untagged)]
enum Interaction {
    ExpectMilestone(ExpectMilestoneInteraction),
    Write(WriteInteraction),
    WriteLine(WriteLineInteraction),
    WriteKey(WriteKeyInteraction),
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct ExpectMilestoneInteraction {
    #[serde(rename = "expect-milestone")]
    expect_milestone: String,
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct WriteInteraction {
    write: String,
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct WriteLineInteraction {
    #[serde(rename = "write-line")]
    write_line: String,
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
struct WriteKeyInteraction {
    #[serde(rename = "write-key")]
    write_key: WriteKey,
}

#[derive(serde::Deserialize, Debug, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
enum WriteKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
    Space,
    Tab,
    Backspace,
    CtrlC,
}

impl WriteKey {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
            Self::Enter => "enter",
            Self::Escape => "escape",
            Self::Space => "space",
            Self::Tab => "tab",
            Self::Backspace => "backspace",
            Self::CtrlC => "ctrl-c",
        }
    }

    const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Up => b"\x1b[A",
            Self::Down => b"\x1b[B",
            Self::Left => b"\x1b[D",
            Self::Right => b"\x1b[C",
            Self::Enter => b"\r",
            Self::Escape => b"\x1b",
            Self::Space => b" ",
            Self::Tab => b"\t",
            Self::Backspace => b"\x7f",
            Self::CtrlC => b"\x03",
        }
    }
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(untagged)]
enum FlavorSpec {
    One(Flavor),
    Many(Vec<Flavor>),
}

impl FlavorSpec {
    fn flavors(&self) -> Vec<Flavor> {
        match self {
            Self::One(flavor) => vec![*flavor],
            Self::Many(flavors) => {
                assert!(!flavors.is_empty(), "`vp` must name at least one flavor");
                flavors.clone()
            }
        }
    }

    const fn is_multi(&self) -> bool {
        matches!(self, Self::Many(_))
    }
}

#[derive(serde::Deserialize, Debug)]
#[serde(untagged)]
enum PlatformFilter {
    Os(String),
    Detailed { os: String, libc: Option<String> },
}

impl PlatformFilter {
    fn matches_current(&self) -> bool {
        let (os, libc) = match self {
            Self::Os(os) => (os.as_str(), None),
            Self::Detailed { os, libc } => (os.as_str(), libc.as_deref()),
        };
        let os_matches = match os {
            "windows" => cfg!(windows),
            "linux" => cfg!(target_os = "linux"),
            "macos" => cfg!(target_os = "macos"),
            other => panic!("unknown skip-platforms os '{other}'"),
        };
        let libc_matches = match libc {
            None => true,
            Some("musl") => cfg!(target_env = "musl"),
            Some("glibc") => cfg!(target_os = "linux") && !cfg!(target_env = "musl"),
            Some(other) => panic!("unknown skip-platforms libc '{other}'"),
        };
        os_matches && libc_matches
    }
}

#[derive(serde::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    /// Which vp flavor(s) run this case: `"local"`, `"global"`, or a list for
    /// parity cases. The list form registers one trial and one snapshot per
    /// flavor.
    vp: FlavorSpec,
    /// Free-form description rendered under the H1 heading of the snapshot.
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    cwd: String,
    /// Exclude-list of platforms this case does not run on.
    #[serde(default, rename = "skip-platforms")]
    skip_platforms: Vec<PlatformFilter>,
    /// Marks the trial `#[ignore]` (runnable with `cargo test -- --ignored`).
    #[serde(default)]
    ignore: bool,
    /// Serve the packed checkout packages through the local npm registry.
    #[serde(default, rename = "local-registry")]
    local_registry: bool,
    /// Seed the case's `VP_HOME` with an already-provisioned managed JS
    /// runtime (see `flavor::js_runtime_seed_dir`). Default true; runtime
    /// provisioning tests set false to start from a genuinely empty home.
    #[serde(default = "default_true", rename = "seed-runtime")]
    seed_runtime: bool,
    /// Case-wide environment additions on top of the harness baseline.
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// Baseline environment variables to remove for this case.
    #[serde(default, rename = "unset-env")]
    unset_env: Vec<String>,
    steps: Vec<Step>,
    /// Cleanup steps: executed after the case, never snapshotted.
    #[serde(default)]
    after: Vec<Step>,
}

#[derive(serde::Deserialize, Default)]
struct SnapshotsFile {
    #[serde(rename = "case", default)]
    cases: Vec<Case>,
}

/// Fixture folder names and `[[case]].name` values must be made of
/// `[A-Za-z0-9_]` only so trial names round-trip through shell filters
/// and snapshot filenames don't carry whitespace or special characters.
fn assert_identifier_like(kind: &str, value: &str) {
    assert!(
        !value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "{kind} '{value}' must contain only ASCII letters, digits, and '_'"
    );
}

fn load_snapshots_file(fixture_path: &Path) -> SnapshotsFile {
    let cases_toml_path = fixture_path.join("snapshots.toml");
    match std::fs::read_to_string(&cases_toml_path) {
        Ok(content) => toml::from_str(&content)
            .unwrap_or_else(|err| panic!("failed to parse {}: {err}", cases_toml_path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => SnapshotsFile::default(),
        Err(err) => {
            panic!("failed to read {}: {err}", cases_toml_path.display());
        }
    }
}

enum TerminationState {
    Exited(i64),
    TimedOut,
}

/// Render the byte stream produced by `screen_contents_formatted` into a
/// snapshot-friendly string: newlines stay literal, SGR escapes and other
/// bytes outside printable ASCII come out as `\xNN`, `\t`, etc.
fn render_formatted_screen(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'\n' => out.push('\n'),
            _ => out.extend(std::ascii::escape_default(b).map(char::from)),
        }
    }
    out
}

/// Append a fenced markdown block containing `body`.
fn push_fenced_block(out: &mut String, body: &str) {
    let trimmed = body.trim_end_matches(['\n', ' ', '\t']);
    out.push_str("```\n");
    if !trimmed.is_empty() {
        out.push_str(trimmed);
        out.push('\n');
    }
    out.push_str("```\n");
}

/// Per-case isolated state directories: `HOME`, `VP_HOME`, and the npm global
/// prefix all live under one disposable root so nothing leaks between cases
/// or into the developer's real environment.
struct CaseHome {
    home: PathBuf,
}

impl CaseHome {
    fn provision(root: &Path, seed_runtime: bool) -> Self {
        let home = root.join("home");
        let vp_home = home.join(".vite-plus");
        std::fs::create_dir_all(&vp_home).unwrap();
        std::fs::create_dir_all(root.join("npm-global/lib")).unwrap();
        // Best-effort: if linking fails, the case downloads the runtime.
        if seed_runtime && let Some(seed) = flavor::js_runtime_seed_dir() {
            flavor::link_dir(&seed, &vp_home.join("js_runtime"));
        }
        Self { home }
    }

    fn vp_home(&self) -> PathBuf {
        self.home.join(".vite-plus")
    }

    fn npm_prefix(&self) -> PathBuf {
        self.home.parent().unwrap().join("npm-global")
    }
}

/// The environment every step starts from. Deliberately small and fully
/// controlled: no ambient variables survive except through the composed PATH.
/// Notably absent: `CI` and `NO_COLOR`; the PTY makes real interactive
/// behaviour the default, and grid rendering strips styling from snapshots.
fn baseline_env(rt: &FlavorRuntime, case_home: &CaseHome) -> BTreeMap<String, OsString> {
    let mut env: BTreeMap<String, OsString> = BTreeMap::new();
    // The case's VP_HOME/bin comes first so `vp env setup` shims take
    // precedence over the harness-provided tools once a case creates them.
    let mut path_entries = vec![case_home.vp_home().join("bin")];
    path_entries.extend(std::env::split_paths(&rt.path_env));
    env.insert("PATH".into(), std::env::join_paths(path_entries).unwrap());
    // xterm-256color keeps anstream from stripping the OSC 8 milestone
    // sequences the harness synchronizes on.
    env.insert("TERM".into(), "xterm-256color".into());
    env.insert("VP_CLI_TEST".into(), "1".into());
    env.insert("VP_EMIT_MILESTONES".into(), "1".into());
    env.insert("NODE_NO_WARNINGS".into(), "1".into());
    // Legacy-harness parity: `vp migrate` fixtures skip real dependency
    // installs (slow, network-bound). Cases that want real installs unset
    // this via `unset-env`.
    env.insert("VP_SKIP_INSTALL".into(), "1".into());
    env.insert("VP_HOME".into(), case_home.vp_home().into_os_string());
    env.insert("NPM_CONFIG_PREFIX".into(), case_home.npm_prefix().into_os_string());
    if cfg!(windows) {
        env.insert("USERPROFILE".into(), case_home.home.clone().into_os_string());
    } else {
        env.insert("HOME".into(), case_home.home.clone().into_os_string());
    }
    for (key, value) in [
        ("GIT_AUTHOR_NAME", "vite-plus-test"),
        ("GIT_AUTHOR_EMAIL", "test@vite-plus.invalid"),
        ("GIT_COMMITTER_NAME", "vite-plus-test"),
        ("GIT_COMMITTER_EMAIL", "test@vite-plus.invalid"),
    ] {
        env.insert(key.into(), value.into());
    }
    if let Some(js_scripts_dir) = &rt.js_scripts_dir {
        env.insert(
            "VITE_GLOBAL_CLI_JS_SCRIPTS_DIR".into(),
            js_scripts_dir.clone().into_os_string(),
        );
    }
    if cfg!(windows) {
        env.insert(
            "PATHEXT".into(),
            ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC".into(),
        );
        // Forward the Windows env vars Node and Git need for temp dirs and
        // profile discovery.
        for name in [
            "TMP",
            "TEMP",
            "APPDATA",
            "LOCALAPPDATA",
            "PROGRAMDATA",
            "HOMEDRIVE",
            "HOMEPATH",
            "WINDIR",
            "SYSTEMROOT",
            "SYSTEMDRIVE",
            "ProgramFiles",
            "ProgramFiles(x86)",
        ] {
            if let Some(value) = std::env::var_os(name) {
                env.insert(name.into(), value);
            }
        }
    }
    env
}

/// Runs a step with piped stdio (no PTY) for `tty = false` cases. Output is
/// stdout followed by stderr; interleaving is intentionally not modelled.
fn run_step_piped(
    program: &Path,
    args: &[String],
    envs: &BTreeMap<String, OsString>,
    cwd: &Path,
    timeout: Duration,
) -> (TerminationState, String) {
    use std::process::{Command, Stdio};

    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .envs(envs)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", program.display()));

    let mut stdout_pipe = child.stdout.take().unwrap();
    let mut stderr_pipe = child.stderr.take().unwrap();
    let stdout_thread = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut buf = String::new();
        let _ = stdout_pipe.read_to_string(&mut buf);
        buf
    });
    let stderr_thread = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut buf = String::new();
        let _ = stderr_pipe.read_to_string(&mut buf);
        buf
    });

    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait().unwrap() {
            Some(status) => break Some(status),
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };

    let mut output = stdout_thread.join().unwrap();
    output.push_str(&stderr_thread.join().unwrap());
    match status {
        Some(status) => (TerminationState::Exited(i64::from(status.code().unwrap_or(-1))), output),
        None => (TerminationState::TimedOut, output),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "test runner with process management necessarily has many lines"
)]
fn run_case(
    tmpdir: &Path,
    fixture_path: &Path,
    fixture_name: &str,
    case_index: usize,
    case: &Case,
    flavor: Flavor,
    runtime: &FlavorRuntime,
    snapshot_name: &str,
) -> Result<(), String> {
    if case.local_registry {
        return Err("`local-registry = true` is not implemented yet (Phase 1 follow-up)".to_owned());
    }

    let snapshots = snapshot_test::Snapshots::new(fixture_path.join("snapshots"));

    // Copy the fixture to a per-case staging directory so the test runs in
    // isolation and workspace-root discovery doesn't walk past the fixture.
    let case_root = tmpdir.join(format!("{fixture_name}_case_{case_index}_{}", flavor.as_str()));
    let stage = case_root.join("workspace");
    std::fs::create_dir_all(&stage).unwrap();
    // The case definition and recorded snapshots are harness metadata, not
    // part of the workspace under test, so they are never copied in.
    CopyOptions::new()
        .filter(|path, _| Ok(path != Path::new("snapshots") && path != Path::new("snapshots.toml")))
        .copy_tree(fixture_path, &stage)
        .unwrap();

    let case_home = CaseHome::provision(&case_root, case.seed_runtime);

    let mut case_env = baseline_env(runtime, &case_home);
    for key in &case.unset_env {
        case_env.remove(key);
    }
    for (key, value) in &case.env {
        case_env.insert(key.clone(), value.into());
    }

    // Real tools resolve through the case's PATH (may be overridden by the
    // case's own env table).
    let case_path: OsString =
        case_env.get("PATH").cloned().unwrap_or_else(|| runtime.path_env.clone());

    let stage_str = stage.to_str().unwrap().to_owned();
    let home_str = case_home.home.to_str().unwrap().to_owned();
    let repo_root = flavor::repo_root();
    let repo_str = repo_root.to_str().unwrap().to_owned();

    let mut doc = String::new();
    doc.push_str(&format!("# {}\n", case.name));
    if let Some(comment) = case.comment.as_deref() {
        // Normalize CRLF → LF: on Windows, git checkouts with autocrlf embed
        // `\r\n` inside TOML multi-line strings.
        let normalized = {
            use cow_utils::CowUtils as _;
            comment.cow_replace("\r\n", "\n").into_owned()
        };
        let trimmed = normalized.trim_matches('\n');
        if !trimmed.is_empty() {
            doc.push('\n');
            doc.push_str(trimmed);
            doc.push('\n');
        }
    }

    let mut timeout_error: Option<String> = None;
    for (step_index, step) in case.steps.iter().enumerate() {
        let argv = step.argv();
        assert!(!argv.is_empty(), "step argv must not be empty");

        // Most steps add no env of their own; borrow the case env then.
        let step_env_override;
        let step_env: &BTreeMap<String, OsString> = if step.envs().is_empty() {
            &case_env
        } else {
            let mut env = case_env.clone();
            for (k, v) in step.envs() {
                env.insert(k.clone(), v.into());
            }
            step_env_override = env;
            &step_env_override
        };
        // Resolution honors a per-step PATH override, so steps testing shims
        // or custom prefixes run exactly the tool the child would see.
        let step_path = step_env.get("PATH").cloned().unwrap_or_else(|| case_path.clone());
        let program = runtime.resolve_program(&argv[0], &step_path)?;
        let step_cwd = stage.join(step.cwd().unwrap_or(case.cwd.as_str()));
        let timeout = step.timeout();

        let (termination_state, raw_output) = if step.tty() {
            let mut cmd = CommandBuilder::new(&program);
            for arg in &argv[1..] {
                cmd.arg(arg);
            }
            cmd.env_clear();
            for (k, v) in step_env {
                cmd.env(k, v);
            }
            cmd.cwd(&step_cwd);

            let terminal = TestTerminal::spawn(SCREEN_SIZE, cmd).unwrap();
            let mut killer = terminal.child_handle.clone();
            let interactions = step.interactions().to_vec();
            let formatted_snapshot = step.formatted_snapshot();
            let output = Arc::new(Mutex::new(String::new()));
            let output_for_thread = Arc::clone(&output);
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let mut terminal = terminal;

                for interaction in interactions {
                    match interaction {
                        Interaction::ExpectMilestone(expect) => {
                            output_for_thread.lock().unwrap().push_str(&format!(
                                "**→ expect-milestone:** `{}`\n\n",
                                expect.expect_milestone
                            ));
                            let milestone_screen =
                                terminal.reader.expect_milestone(&expect.expect_milestone);
                            let mut output = output_for_thread.lock().unwrap();
                            push_fenced_block(&mut output, &milestone_screen);
                            output.push('\n');
                        }
                        Interaction::Write(write) => {
                            output_for_thread
                                .lock()
                                .unwrap()
                                .push_str(&format!("**← write:** `{}`\n\n", write.write));
                            terminal.writer.write_all(write.write.as_bytes()).unwrap();
                            terminal.writer.flush().unwrap();
                        }
                        Interaction::WriteLine(write_line) => {
                            output_for_thread.lock().unwrap().push_str(&format!(
                                "**← write-line:** `{}`\n\n",
                                write_line.write_line
                            ));
                            terminal.writer.write_line(write_line.write_line.as_bytes()).unwrap();
                        }
                        Interaction::WriteKey(write_key) => {
                            let key_name = write_key.write_key.as_str();
                            output_for_thread
                                .lock()
                                .unwrap()
                                .push_str(&format!("**← write-key:** `{key_name}`\n\n"));
                            terminal.writer.write_all(write_key.write_key.bytes()).unwrap();
                            terminal.writer.flush().unwrap();
                        }
                    }
                }

                let status = terminal.reader.wait_for_exit().unwrap();
                let screen = if formatted_snapshot {
                    render_formatted_screen(&terminal.reader.screen_contents_formatted())
                } else {
                    terminal.reader.screen_contents()
                };

                {
                    let mut output = output_for_thread.lock().unwrap();
                    push_fenced_block(&mut output, &screen);
                }

                let _ = tx.send(i64::from(status.exit_code()));
            });

            match rx.recv_timeout(timeout) {
                Ok(exit_code) => {
                    let output = output.lock().unwrap().clone();
                    (TerminationState::Exited(exit_code), output)
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let _ = killer.kill();
                    let output = output.lock().unwrap().clone();
                    (TerminationState::TimedOut, output)
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("terminal thread panicked");
                }
            }
        } else {
            assert!(
                step.interactions().is_empty(),
                "interactions require a PTY; remove `tty = false` or the interactions"
            );
            let (state, raw) = run_step_piped(&program, &argv[1..], step_env, &step_cwd, timeout);
            let mut block = String::new();
            push_fenced_block(&mut block, &raw);
            (state, block)
        };

        // Blank line separator before every `##`.
        doc.push('\n');
        doc.push_str("## `");
        doc.push_str(&step.display_command_line(&case.cwd));
        doc.push_str("`\n\n");

        if let Some(comment) = step.comment() {
            doc.push_str(comment);
            doc.push_str("\n\n");
        }

        // A hung command must fail the trial in both modes: a timeout can
        // never be recorded or blessed as a baseline, not even with
        // UPDATE_SNAPSHOTS=1. The error is deferred (not returned here) so
        // the case's `after` cleanup still runs first.
        if matches!(termination_state, TerminationState::TimedOut) {
            let redacted = redact_output(
                raw_output,
                &[
                    (stage_str.as_str(), "<workspace>"),
                    (home_str.as_str(), "<home>"),
                    (repo_str.as_str(), "<repo>"),
                ],
                !step.formatted_snapshot(),
            );
            timeout_error = Some(format!(
                "step `{}` timed out after {timeout:?}; partial output:\n{redacted}",
                step.display_command_line(&case.cwd),
            ));
            break;
        }

        if let TerminationState::Exited(exit_code) = &termination_state {
            if *exit_code != 0 {
                doc.push_str(&format!("**Exit code:** {exit_code}\n\n"));
            }
        }

        // `snapshot = false` suppresses the screen only on success; failures
        // always keep their output for diagnosis (legacy ignoreOutput
        // semantics).
        let succeeded = matches!(termination_state, TerminationState::Exited(0));
        if step.snapshot() || !succeeded {
            let redacted = redact_output(
                raw_output,
                &[
                    (stage_str.as_str(), "<workspace>"),
                    (home_str.as_str(), "<home>"),
                    (repo_str.as_str(), "<repo>"),
                ],
                !step.formatted_snapshot(),
            );
            doc.push_str(&redacted);
        }

        // Shell-like `&&` semantics: a failing step stops the case unless it
        // opts out, so later steps never bless output from a broken setup.
        if !succeeded && !step.continue_on_failure() {
            if step_index + 1 < case.steps.len() {
                doc.push_str("\n*(remaining steps skipped: step failed)*\n");
            }
            break;
        }
    }

    // Cleanup steps: best-effort, never snapshotted. Per-step envs apply
    // here too: cleanup often depends on the same PATH/prefix overrides as
    // the step it tears down.
    for step in &case.after {
        let argv = step.argv();
        assert!(!argv.is_empty(), "after-step argv must not be empty");
        let after_env_override;
        let after_env: &BTreeMap<String, OsString> = if step.envs().is_empty() {
            &case_env
        } else {
            let mut env = case_env.clone();
            for (k, v) in step.envs() {
                env.insert(k.clone(), v.into());
            }
            after_env_override = env;
            &after_env_override
        };
        let after_path = after_env.get("PATH").cloned().unwrap_or_else(|| case_path.clone());
        if let Ok(program) = runtime.resolve_program(&argv[0], &after_path) {
            let _ = std::process::Command::new(program)
                .args(&argv[1..])
                .env_clear()
                .envs(after_env)
                .current_dir(stage.join(step.cwd().unwrap_or(case.cwd.as_str())))
                .stdin(std::process::Stdio::null())
                .output();
        }
    }

    // Deferred so the cleanup above always runs, even for hung steps.
    if let Some(error) = timeout_error {
        return Err(error);
    }

    snapshots.check_snapshot(snapshot_name, &doc)
}

fn main() {
    let tmp_dir = tempfile::tempdir().unwrap();
    // dunce, not std: std's canonicalize returns a `\\?\` verbatim path on
    // Windows, and CMD.EXE (which runs the local flavor's .cmd shims)
    // rejects verbatim/UNC working directories outright.
    let tmp_dir_path: Arc<Path> = Arc::from(dunce::canonicalize(tmp_dir.path()).unwrap());

    // Bare `vite-plus` / `@voidzero-dev/vite-plus-core` imports in fixture
    // configs resolve to the checkout packages via Node's upward walk (the
    // staged workspaces have no node_modules of their own); the linked
    // packages' own dependencies then resolve at their real location.
    // Anything else a fixture imports must be vendored inside the fixture.
    {
        let repo_root = flavor::repo_root();
        let node_modules = tmp_dir_path.join("node_modules");
        let scoped = node_modules.join("@voidzero-dev");
        std::fs::create_dir_all(&scoped).unwrap();
        flavor::link_dir(&repo_root.join("packages/cli"), &node_modules.join("vite-plus"));
        flavor::link_dir(&repo_root.join("packages/core"), &scoped.join("vite-plus-core"));
    }

    let fixtures_dir = flavor::manifest_dir().join("tests/cli_snapshots/fixtures");

    let mut fixture_paths = std::fs::read_dir(&fixtures_dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", fixtures_dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|p| {
            p.is_dir()
                && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| !n.starts_with('.'))
        })
        .collect::<Vec<_>>();
    fixture_paths.sort();

    let mut args = libtest_mimic::Arguments::from_args();
    // On Linux, parallel PTY + signal-routing contention makes ctrl-c cases
    // flaky (inherited from the vite-task harness; scoping this serialization
    // is an open question in the RFC).
    if cfg!(target_os = "linux") && args.test_threads.is_none() {
        args.test_threads = Some(1);
    }

    // `VP_SNAP_SKIP_FLAVORS=local` (comma-separated) skips registering trials
    // for a flavor entirely; CI legs that don't build the JS CLI use it.
    let skip_flavors: Vec<String> = std::env::var("VP_SNAP_SKIP_FLAVORS")
        .map(|v| v.split(',').map(|s| s.trim().to_owned()).collect())
        .unwrap_or_default();

    // Provision each flavor at most once per run; individual trials surface
    // the error message when their flavor is unavailable.
    let mut runtimes: BTreeMap<&'static str, Arc<Result<FlavorRuntime, String>>> = BTreeMap::new();
    let mut runtime_for = |flavor: Flavor| -> Arc<Result<FlavorRuntime, String>> {
        let tmp = &tmp_dir_path;
        Arc::clone(
            runtimes
                .entry(flavor.as_str())
                .or_insert_with(|| Arc::new(flavor::provision(flavor, tmp))),
        )
    };

    let mut tests: Vec<libtest_mimic::Trial> = Vec::new();
    for fixture_path in fixture_paths {
        let fixture_path: Arc<Path> = Arc::from(fixture_path.as_path());
        let fixture_name: Arc<str> = Arc::from(fixture_path.file_name().unwrap().to_str().unwrap());
        assert_identifier_like("fixture folder", &fixture_name);
        let cases_file = load_snapshots_file(&fixture_path);
        for (case_index, case) in cases_file.cases.into_iter().enumerate() {
            assert_identifier_like("case name", &case.name);
            if case.skip_platforms.iter().any(PlatformFilter::matches_current) {
                continue;
            }
            let multi = case.vp.is_multi();
            let case = Arc::new(case);
            for flavor in case.vp.flavors() {
                if skip_flavors.iter().any(|s| s == flavor.as_str()) {
                    continue;
                }
                let trial_name = if multi {
                    format!("{fixture_name}::{}::{}", case.name, flavor.as_str())
                } else {
                    format!("{fixture_name}::{}", case.name)
                };
                let snapshot_name = if multi {
                    format!("{}.{}.md", case.name, flavor.as_str())
                } else {
                    format!("{}.md", case.name)
                };
                let runtime = runtime_for(flavor);
                let fixture_path = Arc::clone(&fixture_path);
                let fixture_name = Arc::clone(&fixture_name);
                let tmp_dir_path = Arc::clone(&tmp_dir_path);
                let case = Arc::clone(&case);
                let ignored = case.ignore;
                tests.push(
                    libtest_mimic::Trial::test(trial_name, move || {
                        let runtime = match &*runtime {
                            Ok(runtime) => runtime,
                            Err(message) => return Err(message.clone().into()),
                        };
                        run_case(
                            &tmp_dir_path,
                            &fixture_path,
                            &fixture_name,
                            case_index,
                            &case,
                            flavor,
                            runtime,
                            &snapshot_name,
                        )
                        .map_err(Into::into)
                    })
                    .with_ignored_flag(ignored),
                );
            }
        }
    }

    let conclusion = libtest_mimic::run(&args, tests);
    // exit() never returns, so the staged run tree must be dropped first or
    // every run would leave its full tempdir behind.
    drop(tmp_dir);
    conclusion.exit();
}
