//! End to end: the stash promise against the real binary. The app
//! starts on a virtual display, its process dies, and its shell must
//! survive in tmux and be reattached — not recreated — on the next
//! start. Everything runs in a private HOME, runtime dir and tmux dir.
//!
//! Needs `xvfb-run` (with `xauth`), `dbus-run-session` and `tmux`.
//! Without them the test is skipped, unless `STASHEE_E2E=require`
//! (set in CI) turns the skip into a failure.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TOOLS: [&str; 4] = ["xvfb-run", "xauth", "dbus-run-session", "tmux"];

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        // Short on purpose: unix socket paths are capped at 108 bytes.
        let root = std::env::temp_dir().join(format!("stashee-e2e-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for dir in ["home", "run", "tmux"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        for dir in ["run", "tmux"] {
            fs::set_permissions(root.join(dir), fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self { root }
    }

    fn env(&self, command: &mut Command) {
        let home = self.root.join("home");
        command
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("TMUX_TMPDIR", self.root.join("tmux"))
            .env("SHELL", "/bin/sh")
            .env("GDK_BACKEND", "x11")
            .env("GSK_RENDERER", "cairo")
            .env("GTK_A11Y", "none")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("TMUX");
    }

    /// The app on its own display and session bus, as the leader of a
    /// new process group so the whole stack can be killed at once.
    fn launch(&self) -> Child {
        let log = fs::File::options()
            .create(true)
            .append(true)
            .open(self.root.join("app.log"))
            .unwrap();
        let mut command = Command::new("xvfb-run");
        command
            .args(["-a", "-s", "-screen 0 1280x800x24 -nolisten tcp"])
            .args(["dbus-run-session", "--", env!("CARGO_BIN_EXE_stashee")])
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .process_group(0);
        self.env(&mut command);
        command.spawn().expect("xvfb-run starts")
    }

    /// `(name, attached clients)` for every session on our socket.
    fn sessions(&self) -> Vec<(String, u32)> {
        let mut command = Command::new("tmux");
        command.args([
            "-L",
            "stashee",
            "list-sessions",
            "-F",
            "#{session_name} #{session_attached}",
        ]);
        self.env(&mut command);
        let Ok(output) = command.output() else {
            return Vec::new();
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let (name, attached) = line.rsplit_once(' ')?;
                Some((name.to_owned(), attached.parse().ok()?))
            })
            .collect()
    }

    fn wait_for(&self, what: &str, mut done: impl FnMut(&[(String, u32)]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let sessions = self.sessions();
            if done(&sessions) {
                return;
            }
            if Instant::now() > deadline {
                let log = fs::read_to_string(self.root.join("app.log")).unwrap_or_default();
                panic!("timed out waiting for {what}; sessions: {sessions:?}\napp log:\n{log}");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// SIGTERM the app's whole process group: xvfb-run, Xvfb, the bus, the
/// app and its tmux clients. The tmux server daemonized into its own
/// session and is not in the group — that is the point. Never a `kill`
/// command here: see `stashee_pty::terminate_group`.
fn kill(mut app: Child) {
    stashee_pty::terminate_group(app.id()).unwrap();
    let _ = app.wait();
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let mut command = Command::new("tmux");
        command.args(["-L", "stashee", "kill-server"]);
        self.env(&mut command);
        let _ = command.output();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn missing_tools() -> Vec<&'static str> {
    TOOLS
        .into_iter()
        .filter(|tool| {
            Command::new("sh")
                .args(["-c", &format!("command -v {tool}")])
                .stdout(Stdio::null())
                .status()
                .map_or(true, |status| !status.success())
        })
        .collect()
}

#[test]
fn a_killed_app_leaves_its_shell_running_and_reattaches_it() {
    let missing = missing_tools();
    if !missing.is_empty() {
        assert!(
            std::env::var("STASHEE_E2E").as_deref() != Ok("require"),
            "STASHEE_E2E=require but missing: {missing:?}"
        );
        eprintln!("skipping: missing {missing:?}");
        return;
    }
    let sandbox = Sandbox::new();

    // First start: the Welcome workflow's one pane, attached.
    let app = sandbox.launch();
    sandbox.wait_for("the first pane to attach", |sessions| {
        sessions.len() == 1 && sessions[0].1 > 0
    });
    let session = sandbox.sessions()[0].0.clone();
    assert!(session.starts_with("stashee-welcome-"), "{session}");

    kill(app);
    sandbox.wait_for("the client to detach", |sessions| {
        sessions == [(session.clone(), 0)]
    });

    // Second start: the same session comes back, nothing new is made.
    let app = sandbox.launch();
    sandbox.wait_for("the pane to reattach", |sessions| {
        sessions.len() == 1 && sessions[0].0 == session && sessions[0].1 > 0
    });
    let state = fs::read_to_string(sandbox.root.join("home/.local/share/stashee/state.toml"))
        .expect("state saved");
    let pane_id = session.rsplit('-').next().unwrap();
    assert!(state.contains(pane_id), "state lost the pane:\n{state}");
    kill(app);
}
