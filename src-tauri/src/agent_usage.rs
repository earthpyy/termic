//! Subscription usage: how much of an account's rolling limits is spent
//! (GH #277).
//!
//! Two providers, two transports, and they are different on purpose.
//!
//! **claude** reports itself. Claude Code pipes `rate_limits` into the
//! statusLine command's stdin on every turn, so `agent_hooks::statusline_body`
//! installs a script that forwards it over the OSC channel the hooks already
//! use. Nothing in this module is involved: the numbers arrive at the terminal.
//!
//! **codex** is asked. It exposes `account/rateLimits/read` as a documented
//! JSON-RPC method on `codex app-server`, which answers COLD, with no agent
//! running and no session in flight, and returns the account id alongside the
//! numbers. That is strictly better than the alternative (tailing the
//! `token_count` event out of its rollout JSONL): no file walking, no parsing,
//! and no dependence on `session_meta.cwd`, which inside Docker is the path as
//! the CONTAINER sees it and cannot be matched against a host worktree.
//!
//! Both are per-account for the same reason: a cloned agent relocates its whole
//! config dir with the agent's own env var (`CODEX_HOME` here), so the config
//! dir IS the account. This module never has to know what an account is; it
//! points codex at a directory and believes what comes back.
//!
//! See docs/ideas/usage-footer.md for the sources that were measured and
//! rejected, which is most of them.

use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// How long to wait for `codex app-server` to answer. Generous for the same
/// reason `codex_trust::LIST_TIMEOUT` is: this is a COLD process start, and the
/// app-server loads config and resolves auth before it answers anything.
const RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Anything at least this long is the WEEKLY window rather than the session
/// one. 7 days in minutes. Codex reports durations rather than names, and a
/// free plan reports a single 30-day window, so the split has to be made here.
const WEEKLY_MIN_MINUTES: u64 = 7 * 24 * 60;

/// One rolling limit window.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    /// 0-100, clamped. A provider reporting 101 must not overflow a bar.
    pub used_percent: f64,
    /// Unix epoch SECONDS, or None when the provider did not say.
    pub resets_at: Option<i64>,
}

/// What one account has spent. Either window can be absent: codex on a free
/// plan reports a single 30-day window and no second one at all.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsage {
    pub session: Option<UsageWindow>,
    pub weekly: Option<UsageWindow>,
    /// `free`, `plus`, `pro`, `team`… Passed through verbatim rather than
    /// matched: the enum has a dozen arms and gains more.
    pub plan_type: Option<String>,
    /// Which account answered. Not displayed, but it is the only way to tell
    /// two clones apart in a log when one of them shows the wrong number.
    pub account_id: Option<String>,
}

/// Read one window out of codex's JSON. Returns None for a null window, which
/// is the normal shape of `secondary` rather than an error.
fn window(v: &serde_json::Value) -> Option<(UsageWindow, u64)> {
    let obj = v.as_object()?;
    let used = obj.get("usedPercent").and_then(serde_json::Value::as_f64)?;
    // Absent duration is treated as the SHORT window, because that is the one
    // a footer leads with and mislabelling it costs less than dropping it.
    let mins = obj
        .get("windowDurationMins")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let resets_at = obj.get("resetsAt").and_then(serde_json::Value::as_i64);
    Some((
        UsageWindow { used_percent: used.clamp(0.0, 100.0), resets_at },
        mins,
    ))
}

/// Sort codex's `primary`/`secondary` into session/weekly BY DURATION.
///
/// Not by position. Codex names them by precedence, not by length, and a free
/// plan sends a single 30-day window as `primary` with no `secondary` at all.
/// Reading position as meaning would file that 30-day window under "5h" and
/// paint a session bar that resets next month.
pub fn classify(primary: Option<(UsageWindow, u64)>, secondary: Option<(UsageWindow, u64)>)
    -> (Option<UsageWindow>, Option<UsageWindow>)
{
    let mut session = None;
    let mut weekly = None;
    for (win, mins) in [primary, secondary].into_iter().flatten() {
        let slot = if mins >= WEEKLY_MIN_MINUTES { &mut weekly } else { &mut session };
        // Two windows landing in the same slot keeps the SHORTER one, so a plan
        // reporting 7d and 30d does not show whichever arrived last.
        if slot.is_none() {
            *slot = Some(win);
        }
    }
    (session, weekly)
}

/// Parse the `result` of `account/rateLimits/read`.
pub fn parse_codex_result(result: &serde_json::Value) -> AgentUsage {
    let rl = result.get("rateLimits");
    let (session, weekly) = match rl {
        Some(rl) => classify(
            rl.get("primary").and_then(window),
            rl.get("secondary").and_then(window),
        ),
        None => (None, None),
    };
    AgentUsage {
        session,
        weekly,
        plan_type: rl
            .and_then(|r| r.get("planType"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        account_id: result
            .get("accountId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    }
}

/// The `codex app-server` invocation, built separately so a test can read its
/// ENVIRONMENT back without needing a codex on the machine.
///
/// It exists because of a bug that shipped. A packaged `.app` is launched by
/// the GUI with `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, and codex installs to
/// `~/.local/bin`, so `Command::new("codex")` is a plain ENOENT in every
/// release build while working in every dev one, where the terminal's PATH is
/// inherited. The frontend swallows this error on purpose, so the footer was
/// simply empty for codex with nothing anywhere saying why.
///
/// `shell_env` exists for exactly this and its module doc says so; the fix is
/// to use it, and the test below is what stops it being dropped again.
fn app_server_command(bin: &str, home: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.arg("app-server")
        .env("PATH", crate::shell_env::resolved_path())
        .env("CODEX_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    cmd
}

/// One `initialize` + one `account/rateLimits/read` over stdio, then kill the
/// child.
///
/// Deliberately the same shape as `codex_trust::hooks_list`, down to the worker
/// thread and the kill-first-report-second ordering, because it is the same
/// hazard: the app-server is a long-lived JSON-RPC peer, termic wants one
/// answer, and a child that never answers would otherwise hang a caller with
/// the pipe still open.
pub fn fetch_codex(agent_id: &str, docker: bool) -> Result<AgentUsage, String> {
    let home = codex_home(agent_id, docker)?;
    let bin = codex_binary(agent_id);

    let mut child = app_server_command(&bin, &home)
        .spawn()
        .map_err(|e| {
            // Logged, not just returned. The caller swallows this error on
            // purpose (a missing codex must not raise a banner over a footer
            // number), so without a trace here a total failure is invisible,
            // which is precisely how it reached a release.
            let msg = format!("could not start `{bin} app-server`: {e}");
            crate::dlog(&format!("[agent-usage] {msg}"));
            msg
        })?;

    {
        let stdin = child.stdin.as_mut().ok_or("no stdin on codex app-server")?;
        let init = serde_json::json!({
            "id": 1, "method": "initialize",
            "params": { "clientInfo": { "name": "termic", "version": env!("CARGO_PKG_VERSION") } }
        });
        let read = serde_json::json!({ "id": 2, "method": "account/rateLimits/read", "params": {} });
        for msg in [init, read] {
            writeln!(stdin, "{msg}").map_err(|e| format!("write to codex app-server: {e}"))?;
        }
        stdin.flush().map_err(|e| format!("flush codex app-server: {e}"))?;
    }

    let stdout = child.stdout.take().ok_or("no stdout on codex app-server")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            // Responses only. The app-server also streams notifications, which
            // carry no `id` and are none of our business here.
            if v.get("id").and_then(serde_json::Value::as_i64) == Some(2) {
                let _ = tx.send(v);
                return;
            }
        }
    });

    let answer = rx.recv_timeout(RPC_TIMEOUT);
    let _ = child.kill();
    let _ = child.wait();
    let answer = answer.map_err(|_| {
        format!(
            "`{bin} app-server` did not answer account/rateLimits/read within {}s",
            RPC_TIMEOUT.as_secs()
        )
    })?;

    if let Some(err) = answer.get("error") {
        return Err(format!("codex refused account/rateLimits/read: {err}"));
    }
    let result = answer
        .get("result")
        .ok_or("codex answered account/rateLimits/read with no result")?;
    Ok(parse_codex_result(result))
}

/// Where codex keeps ITS config for this agent entry. `CODEX_HOME` is how a
/// clone points codex at a second account, and `instance_config_dir` already
/// resolves that from the agent ENTRY, so a clone is asked about its own login
/// rather than the base's.
fn codex_home(agent_id: &str, docker: bool) -> Result<PathBuf, String> {
    // A DOCKER task's codex logs in inside the container, whose CODEX_HOME is
    // the termic-owned directory bind-mounted at that path. Asking the host's
    // `~/.codex` instead would report a DIFFERENT ACCOUNT's quota under the
    // task's name, which is worse than reporting none: the number looks
    // authoritative and belongs to someone else's login.
    if docker {
        return Ok(crate::docker::agent_config_host_dir(agent_id));
    }
    let home = dirs::home_dir().ok_or("no home dir")?;
    let agents = crate::load_settings_inner().agents;
    crate::agent_dirs::instance_config_dir(&agents, agent_id, &home)
        .ok_or_else(|| format!("{agent_id} has no known state dir"))
}

/// The codex binary to ask. Resolved from the REGISTRY entry rather than
/// hard-coded, so a user who renamed the command or pointed it at an absolute
/// path gets their binary asked.
fn codex_binary(agent_id: &str) -> String {
    let agents = crate::load_settings_inner().agents;
    crate::agent_dirs::resolve_agent(&agents, agent_id)
        .map(|a| a.command)
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| "codex".to_string())
}

/// Ask codex for this agent entry's usage.
///
/// `async` and off the main thread, because it SPAWNS A PROCESS and waits up to
/// 10s for it: a synchronous Tauri command doing that blocks the WKWebView
/// event loop and freezes the whole window (see CLAUDE.md).
#[tauri::command]
pub async fn agent_usage_codex(agent_id: String, docker: bool) -> Result<AgentUsage, String> {
    tauri::async_runtime::spawn_blocking(move || fetch_codex(&agent_id, docker))
        .await
        .map_err(|e| format!("usage task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape codex answers with on a paid plan: a short window and a long
    /// one. Transcribed from the protocol schema, not pasted from a real
    /// account (see CLAUDE.md on fixtures).
    fn paid() -> serde_json::Value {
        serde_json::json!({
            "rateLimits": {
                "primary":   { "usedPercent": 58.0, "windowDurationMins": 300, "resetsAt": 1790000000i64 },
                "secondary": { "usedPercent": 41.0, "windowDurationMins": 10080, "resetsAt": 1790500000i64 },
                "planType": "pro"
            },
            "accountId": "00000000-0000-0000-0000-000000000000"
        })
    }

    /// The RPC against a REAL codex, end to end: spawn, handshake, method,
    /// parse. Everything above this tests the parse alone, and the parse is not
    /// where this breaks. The method name, the handshake shape and whether the
    /// app-server answers a client it has never seen are the parts that can be
    /// wrong, and only a live binary can say.
    ///
    /// `#[ignore]`d for the same reason `codex_hooks_install_...` is: CI has no
    /// codex on PATH and no login. Run it against a real one with
    ///
    /// ```sh
    /// cargo test codex_rate_limits_live -- --ignored --nocapture
    /// ```
    ///
    /// It asserts SHAPE, never a number: the percentages belong to whoever runs
    /// it and change by the hour.
    #[test]
    #[ignore = "needs a real, logged-in codex binary on PATH"]
    fn codex_rate_limits_live() {
        let usage = fetch_codex("codex", false).expect("codex should answer account/rateLimits/read");
        println!("{}", serde_json::to_string_pretty(&usage).unwrap());
        assert!(
            usage.session.is_some() || usage.weekly.is_some(),
            "a logged-in account reports at least one window"
        );
        for w in [usage.session.as_ref(), usage.weekly.as_ref()].into_iter().flatten() {
            assert!((0.0..=100.0).contains(&w.used_percent), "{w:?}");
        }
    }

    /// The regression that shipped in 1.2.1: the spawn inherited the GUI's
    /// minimal PATH, could not find `codex` in `~/.local/bin`, and the footer
    /// was silently empty in every release build.
    ///
    /// Asserted on the COMMAND rather than by running codex, so it holds on a
    /// machine that has none, which is every CI runner.
    #[test]
    fn the_app_server_spawn_carries_the_login_path() {
        let cmd = app_server_command("codex", Path::new("/Users/u/.codex"));
        let envs: Vec<_> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(),
                           v.map(|v| v.to_string_lossy().into_owned())))
            .collect();
        let path = envs.iter().find(|(k, _)| k == "PATH")
            .expect("PATH must be set explicitly, never inherited from the GUI");
        assert!(
            path.1.as_deref().is_some_and(|p| !p.is_empty()),
            "PATH was set to nothing, which is worse than not setting it"
        );
        assert!(
            envs.iter().any(|(k, v)| k == "CODEX_HOME"
                && v.as_deref() == Some("/Users/u/.codex")),
            "CODEX_HOME is how a clone is asked about its own login: {envs:?}"
        );
    }

    /// A Docker task's codex logs in INSIDE the container, against the config
    /// dir termic mounts there. Asking the host's `~/.codex` would report a
    /// different account's quota under this task's name, which is worse than
    /// reporting none: it looks authoritative and belongs to someone else.
    #[test]
    fn a_docker_task_is_asked_about_the_mounted_config_dir() {
        let docker = codex_home("codex", true).expect("docker dir always resolves");
        assert!(
            docker.ends_with("docker-agents/codex"),
            "expected the termic-owned mounted dir, got {}",
            docker.display()
        );
        if let Ok(host) = codex_home("codex", false) {
            assert_ne!(host, docker);
        }
    }

    #[test]
    fn sorts_the_two_windows_by_duration() {
        let u = parse_codex_result(&paid());
        assert_eq!(u.session.as_ref().unwrap().used_percent, 58.0);
        assert_eq!(u.weekly.as_ref().unwrap().used_percent, 41.0);
        assert_eq!(u.session.as_ref().unwrap().resets_at, Some(1790000000));
        assert_eq!(u.plan_type.as_deref(), Some("pro"));
        assert!(u.account_id.is_some());
    }

    /// The bug this exists to prevent: reading `primary` as "the session
    /// window" files a free plan's 30-day window under 5h and paints a session
    /// bar that resets next month.
    #[test]
    fn a_free_plans_single_long_window_is_weekly_not_session() {
        let free = serde_json::json!({
            "rateLimits": {
                "primary": { "usedPercent": 49.0, "windowDurationMins": 43200, "resetsAt": 1790491695i64 },
                "secondary": serde_json::Value::Null,
                "planType": "free"
            }
        });
        let u = parse_codex_result(&free);
        assert!(u.session.is_none(), "a 30-day window is not a session window");
        assert_eq!(u.weekly.unwrap().used_percent, 49.0);
        assert_eq!(u.plan_type.as_deref(), Some("free"));
    }

    #[test]
    fn a_null_or_missing_rate_limits_is_empty_not_an_error() {
        assert_eq!(parse_codex_result(&serde_json::json!({})), AgentUsage::default());
        let no_windows = serde_json::json!({ "rateLimits": { "primary": null, "secondary": null } });
        let u = parse_codex_result(&no_windows);
        assert!(u.session.is_none() && u.weekly.is_none());
    }

    #[test]
    fn percentages_are_clamped_so_a_bar_cannot_overflow() {
        let over = serde_json::json!({
            "rateLimits": { "primary": { "usedPercent": 140.0, "windowDurationMins": 300 } }
        });
        assert_eq!(parse_codex_result(&over).session.unwrap().used_percent, 100.0);
    }

    /// A window with no duration at all still shows up, as the session one.
    /// Dropping it would be a silently empty footer on a schema change.
    #[test]
    fn a_window_without_a_duration_degrades_to_session() {
        let bare = serde_json::json!({
            "rateLimits": { "primary": { "usedPercent": 12.0 } }
        });
        let u = parse_codex_result(&bare);
        assert_eq!(u.session.unwrap().used_percent, 12.0);
        assert!(u.weekly.is_none());
    }
}
