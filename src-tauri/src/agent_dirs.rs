//! Single source of truth for "where does this agent's persistent state
//! actually live". Two very different consumers used to hand-maintain
//! their own copy of this and could silently drift apart:
//!
//! - Seatbelt's default `Agent.sandbox_allowed_paths` (`lib.rs`'s
//!   `default_agents()`) — real `$HOME` paths on the host, allow-listed
//!   for `sandbox-exec` to read/write directly.
//! - Docker's per-agent config-dir mount (`docker.rs`'s `agent_config()`)
//!   — container `/root` paths, bind-mounted from a termic-owned host dir
//!   that is never the host's real `$HOME`.
//!
//! Docker only wants the CONFIRMED state dirs (login, sessions, MCP
//! config — the ones `docs/plans/docker-sandbox/findings.md` actually
//! verified hold real state): it mounts a termic-owned dir, not the real
//! `$HOME`, so persisting a cache dir there buys nothing. Seatbelt allows
//! these same dirs, PLUS its own macOS-only extras (XDG-style
//! `.config`/`.local/share` paths some agents may or may not ever use,
//! `Library/Application Support/*`, regex-covered sidecar files like
//! claude's `.claude.json`) that have no Docker-container equivalent and
//! stay hand-authored in `default_agents()`.
//!
//! Keeping the CONFIRMED subset here means a renamed or added state dir
//! is a one-line change in one place, not two files quietly falling out
//! of sync.

/// One agent's confirmed state dirs, relative to its home (`$HOME` on the
/// host, `/root` inside the Docker image — both conventions land on the
/// same relative subpath). Order matters for an agent with no config-dir
/// relocation env var: the FIRST entry is Docker's primary mount, every
/// entry after it is an `extra_dirs` mount alongside it.
/// Where THIS agent INSTANCE keeps its config on the host.
///
/// One resolver rather than a per-agent abstraction, deliberately. The pieces
/// are already data (`state_dirs`, `config_relocation_env`, and the hook
/// installer's `settings_rel`), and the only thing missing was somewhere that
/// composes them for a specific agent entry rather than for a built-in NAME.
/// A trait or a module per agent would buy no control that these tables do not
/// already give, and would turn "add an agent" from adding a row into
/// implementing an interface.
///
/// Three cases, most specific first:
///   1. the entry relocates its whole config with the agent's own env var
///      (`CLAUDE_CONFIG_DIR=~/.next-claude`), which is how a clone holds a
///      SECOND account. That path is the config dir, verbatim.
///   2. the entry overrides `HOME`, so the default dir hangs off that instead.
///   3. neither: the base's default dir under the real home.
///
/// Returns None only when the base agent has no known state dir at all, which
/// is the honest answer for an agent nobody has mapped.
pub fn instance_config_dir(
    agents: &[crate::Agent],
    agent_id: &str,
    home: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let base = crate::docker::base_agent_id(agents, agent_id);
    let entry = agents.iter().find(|a| a.id == agent_id);
    if let Some(env) = entry.map(|a| &a.env) {
        if let Some(raw) = config_relocation_env(base).and_then(|var| env.get(var)) {
            let expanded = expand_home(raw, home);
            if !expanded.as_os_str().is_empty() {
                return Some(expanded);
            }
        }
        if let Some(h) = env.get("HOME").filter(|h| !h.is_empty()) {
            return Some(std::path::Path::new(h).join(state_dirs(base).first()?));
        }
    }
    Some(home.join(state_dirs(base).first()?))
}

/// `~` and `$HOME` in a user-typed env value. They type these by hand in
/// Settings, so a literal `~/.next-claude` has to become a real path rather
/// than a directory called `~` (which is what the file tree in the reporter's
/// screenshot was already showing).
fn expand_home(raw: &str, home: &std::path::Path) -> std::path::PathBuf {
    let t = raw.trim();
    if t == "~" || t == "$HOME" {
        return home.to_path_buf();
    }
    for prefix in ["~/", "$HOME/"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return home.join(rest);
        }
    }
    std::path::PathBuf::from(t)
}

/// A clone resolved against what it extends: every field it left EMPTY comes
/// from the parent, live, at read time.
///
/// A clone used to be a full COPY of the parent, made once. That is a snapshot
/// that rots: when a vendor renames a flag the built-in entry moves with the
/// app and every clone keeps the old value forever, silently, with no way for
/// the user to tell which of its seventeen fields they actually chose. It had
/// already happened here, a clone carrying the parent's literal `$HOME/.claude`
/// sandbox paths while its own config lived elsewhere, so the cage denied it
/// its own login.
///
/// EMPTY MEANS INHERIT, which is the rule `classifyAgentTitle` already uses for
/// per-field signal fallback, extended to the whole record rather than a second
/// convention. Cost of the choice, and it is the same one that doc records: "no
/// value at all" stops being expressible by clearing a field, because clearing
/// is how you ask for the parent's.
///
/// `id`, `extends`, `display_name` and `builtin` are the clone's OWN identity
/// and are never inherited. Resolution walks the chain, so a clone of a clone
/// works, and is depth-capped because ids are user-editable.
/// Per-LIST capability merge. Wholesale replacement would mean a clone that
/// overrides one flag list stops tracking the parent on every other, which is
/// the freeze this change exists to remove, one field down.
fn merge_caps(child: &mut crate::AgentCapabilities, parent: &crate::AgentCapabilities) {
    macro_rules! take_if_empty {
        ($($f:ident),* $(,)?) => { $( if child.$f.is_empty() { child.$f = parent.$f.clone(); } )* };
    }
    take_if_empty!(
        yolo_args, runtime_yolo_command, runtime_default_command,
        resume_args, session_id_args, resume_id_args, name_args,
    );
    // Signals are per-FIELD, matching `classifyAgentTitle`: overriding the
    // busy patterns must not silently drop the inherited idle ones.
    if child.signals.busy.is_empty() { child.signals.busy = parent.signals.busy.clone(); }
    if child.signals.idle.is_empty() { child.signals.idle = parent.signals.idle.clone(); }
    if child.signals.attention.is_empty() {
        child.signals.attention = parent.signals.attention.clone();
    }
    if child.signals.pending.is_empty() {
        child.signals.pending = parent.signals.pending.clone();
    }
}

/// A clone resolved against what it extends: every field it left EMPTY comes
/// from the parent, live, at read time.
///
/// A clone used to be a full COPY of the parent, made once. That is a snapshot
/// that rots: when a vendor renames a flag the built-in entry moves with the
/// app and every clone keeps the old value forever, silently, with no way for
/// the user to tell which of its seventeen fields they actually chose. It had
/// already happened here, a clone carrying the parent's literal `$HOME/.claude`
/// sandbox paths while its own config lived elsewhere, so the cage denied it
/// its own login.
///
/// EMPTY MEANS INHERIT, which is the rule `classifyAgentTitle` already uses for
/// per-field signal fallback, extended to the whole record rather than a second
/// convention. Cost of the choice, and it is the same one that doc records: "no
/// value at all" stops being expressible by clearing a field, because clearing
/// is how you ask for the parent's.
///
/// `id`, `extends`, `display_name` and `builtin` are the clone's OWN identity
/// and are never inherited. Resolution walks the chain, so a clone of a clone
/// works, and is depth-capped because ids are user-editable.


pub fn resolve_agent(agents: &[crate::Agent], id: &str) -> Option<crate::Agent> {
    let mut out = agents.iter().find(|a| a.id == id)?.clone();
    let mut cur = out.extends.clone();
    for _ in 0..8 {
        let Some(parent_id) = cur.filter(|p| !p.is_empty() && *p != out.id) else { break };
        let Some(parent) = agents.iter().find(|a| a.id == parent_id) else { break };
        if out.command.trim().is_empty() { out.command = parent.command.clone(); }
        if out.args.is_empty() { out.args = parent.args.clone(); }
        if out.icon_id.trim().is_empty() { out.icon_id = parent.icon_id.clone(); }
        if out.color.trim().is_empty() { out.color = parent.color.clone(); }
        if out.env.is_empty() { out.env = parent.env.clone(); }
        if out.docker_env.is_empty() { out.docker_env = parent.docker_env.clone(); }
        if out.sandbox_allowed_paths.is_empty() {
            out.sandbox_allowed_paths = parent.sandbox_allowed_paths.clone();
        }
        if out.sandbox_allowed_hosts.is_empty() {
            out.sandbox_allowed_hosts = parent.sandbox_allowed_hosts.clone();
        }
        if out.post_launch_capture.is_none() {
            out.post_launch_capture = parent.post_launch_capture.clone();
        }
        // Capabilities are the flags a vendor renames, so this is the field
        // the whole change is FOR. Merged per-list rather than wholesale: a
        // clone overriding `yolo_args` alone must still track the parent's
        // resume flags, or overriding one field silently freezes the rest.
        merge_caps(&mut out.capabilities, &parent.capabilities);
        if !out.work_done { out.work_done = parent.work_done; }
        cur = parent.extends.clone();
    }
    Some(out)
}

/// The env var that relocates an agent's ENTIRE config dir, when it has one.
///
/// This is how a duplicated agent holds a second account: the clone runs the
/// same binary with `CLAUDE_CONFIG_DIR` pointing somewhere else, so its login,
/// its settings and its hooks all live apart from the original's. Anything
/// keyed on the agent's default dir would put one account's hooks into the
/// other account's config, which is worse than not installing them.
///
/// Only agents that genuinely relocate everything are listed. grok has no clean
/// relocation env (binary, skills and config all share `~/.grok`), and the
/// others fold their HOME-root dotfiles into the same dir once relocated.
pub fn config_relocation_env(base_id: &str) -> Option<&'static str> {
    match base_id {
        "claude" => Some("CLAUDE_CONFIG_DIR"),
        "codex" => Some("CODEX_HOME"),
        _ => None,
    }
}

pub fn state_dirs(agent_id: &str) -> &'static [&'static str] {
    match agent_id {
        // claude and codex relocate their ENTIRE config dir via an env var
        // (CLAUDE_CONFIG_DIR / CODEX_HOME — see docker.rs's `agent_config`),
        // which folds HOME-root dotfiles in too (claude's `.claude.json`
        // sits inside `$CLAUDE_CONFIG_DIR` once relocated) — one dir covers
        // everything, so there is nothing else to list.
        "claude" => &[".claude"],
        "codex" => &[".codex"],
        "copilot" => &[".copilot"],
        // agy shares the `.gemini` config shape (Gemini-family CLI) plus
        // its own `.antigravity`.
        "agy" | "antigravity" => &[".gemini", ".antigravity"],
        // opencode follows XDG: config in `.config/opencode`, auth +
        // session DB in `.local/share/opencode`.
        "opencode" => &[".config/opencode", ".local/share/opencode"],
        // pi (Earendil): global settings + trust file live under
        // `~/.pi/agent/`, so the whole `.pi` tree is the config dir. Safe to
        // mount in Docker ONLY because the image installs pi from npm (the
        // binary lands in the global prefix, outside HOME) - pi's own
        // install.sh can put it in `~/.pi/agent/bin`, which would be grok's
        // situation exactly. See assets/Dockerfile.default.
        "pi" => &[".pi"],
        // grok: binary, bundled skills, and config all live under `.grok`
        // with no clean relocation env. Listed here for Seatbelt (which
        // allows the real path regardless); `docker::agent_config` still
        // declines to support it — see findings.md's "outlier" writeup.
        "grok" => &[".grok"],
        _ => &[],
    }
}

#[cfg(test)]
mod instance_dir_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn agent(id: &str, extends: Option<&str>, env: &[(&str, &str)]) -> crate::Agent {
        // Same stub shape docker's tests use: clone a real default rather than
        // construct one, so a new required field cannot silently skip these.
        let mut a = crate::default_agents().into_iter().next().unwrap();
        a.id = id.to_string();
        a.extends = extends.map(|s| s.to_string());
        a.env = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        a
    }
    const HOME: &str = "/Users/u";

    #[test]
    fn a_plain_agent_uses_its_own_default_dir() {
        let agents = vec![agent("claude", None, &[])];
        assert_eq!(
            instance_config_dir(&agents, "claude", Path::new(HOME)),
            Some(PathBuf::from("/Users/u/.claude")),
        );
    }

    #[test]
    fn a_clone_with_no_env_falls_back_to_the_base_dir() {
        // Correct, and worth stating: two agents sharing one login share one
        // config, so they share one set of hooks. Nothing is wrong with that.
        let agents = vec![agent("claude", None, &[]), agent("next-claude", Some("claude"), &[])];
        assert_eq!(
            instance_config_dir(&agents, "next-claude", Path::new(HOME)),
            Some(PathBuf::from("/Users/u/.claude")),
        );
    }

    #[test]
    fn a_clone_holding_a_second_account_gets_its_own_dir() {
        // The reported shape: a second agent entry whose CLAUDE_CONFIG_DIR
        // points outside the first one's default.
        let agents = vec![
            agent("claude", None, &[]),
            agent("next-claude", Some("claude"), &[("CLAUDE_CONFIG_DIR", "/Users/u/.next-claude")]),
        ];
        assert_eq!(
            instance_config_dir(&agents, "next-claude", Path::new(HOME)),
            Some(PathBuf::from("/Users/u/.next-claude")),
        );
        // And the original is untouched, which is the whole point: installing
        // for one account must never write into the other's config.
        assert_eq!(
            instance_config_dir(&agents, "claude", Path::new(HOME)),
            Some(PathBuf::from("/Users/u/.claude")),
        );
    }

    #[test]
    fn a_hand_typed_tilde_is_expanded() {
        // Users type this by hand in Settings, and an unexpanded `~` creates a
        // directory literally called "~".
        for raw in ["~/.next-claude", "$HOME/.next-claude"] {
            let agents = vec![
                agent("claude", None, &[]),
                agent("c2", Some("claude"), &[("CLAUDE_CONFIG_DIR", raw)]),
            ];
            assert_eq!(
                instance_config_dir(&agents, "c2", Path::new(HOME)),
                Some(PathBuf::from("/Users/u/.next-claude")),
                "{raw}",
            );
        }
    }

    #[test]
    fn a_home_override_moves_the_default_dir() {
        let agents = vec![
            agent("claude", None, &[]),
            agent("c2", Some("claude"), &[("HOME", "/tmp/alt")]),
        ];
        assert_eq!(
            instance_config_dir(&agents, "c2", Path::new(HOME)),
            Some(PathBuf::from("/tmp/alt/.claude")),
        );
    }

    #[test]
    fn the_relocation_var_outranks_a_home_override() {
        let agents = vec![
            agent("claude", None, &[]),
            agent("c2", Some("claude"), &[("HOME", "/tmp/alt"), ("CLAUDE_CONFIG_DIR", "/tmp/cfg")]),
        ];
        assert_eq!(
            instance_config_dir(&agents, "c2", Path::new(HOME)),
            Some(PathBuf::from("/tmp/cfg")),
        );
    }

    #[test]
    fn an_agent_with_no_known_dir_says_so() {
        let agents = vec![agent("mystery", None, &[])];
        assert_eq!(instance_config_dir(&agents, "mystery", Path::new(HOME)), None);
    }

    #[test]
    fn only_agents_that_truly_relocate_are_listed() {
        assert_eq!(config_relocation_env("claude"), Some("CLAUDE_CONFIG_DIR"));
        assert_eq!(config_relocation_env("codex"), Some("CODEX_HOME"));
        // grok's binary lives inside its config dir, so it has no clean one.
        assert_eq!(config_relocation_env("grok"), None);
        assert_eq!(config_relocation_env("opencode"), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_agents_have_at_least_one_dir() {
        for id in ["claude", "codex", "copilot", "agy", "antigravity", "opencode", "grok"] {
            assert!(!state_dirs(id).is_empty(), "{id} should list at least one state dir");
        }
    }

    #[test]
    fn unknown_agent_has_no_dirs() {
        assert!(state_dirs("not-a-real-agent").is_empty());
    }

    #[test]
    fn every_entry_is_a_relative_dotfile_path() {
        // Every consumer prefixes these with either "$HOME/" or "/root/",
        // so a leading slash or a bare (non-dotfile) name here would
        // silently produce a wrong mount/allow-list path in both places.
        for id in ["claude", "codex", "copilot", "agy", "opencode", "grok"] {
            for dir in state_dirs(id) {
                assert!(dir.starts_with('.'), "{id}'s {dir} should be a relative dotfile path");
                assert!(!dir.starts_with('/'), "{id}'s {dir} should not be absolute");
            }
        }
    }
}
