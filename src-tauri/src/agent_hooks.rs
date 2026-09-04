//! Agent lifecycle hooks: one hook, installed into Claude's own config, that
//! tells termic the moment the agent is blocked on the user.
//!
//! Why this exists: Claude paints its IDLE glyph while it is waiting on a
//! permission prompt, a question, or plan approval. termic reads that title,
//! arms its 5s settle, and fires a "done" badge about a second before the
//! native OSC 9 notify (6.0s late) corrects it to "needs you". Measured; see
//! `docs/agent-hooks.md`.
//!
//! The transport is deliberately not IPC. A Claude hook's stdout JSON may carry
//! a `terminalSequence`, which Claude writes to its own PTY, and `TerminalPane`
//! already parses OSC 777 into `goAttention` (which calls `cancelSettle`, and
//! that is what kills the false done). So there is no socket, no callback
//! binary, no Seatbelt grant and no Docker plumbing: the channel is the terminal
//! the agent already owns, and it behaves identically caged and uncaged.
//!
//! Two things here are load-bearing and easy to undo by accident:
//!
//! 1. The script lives in the agent's OWN config dir, never the termic data
//!    dir. Seatbelt denies the data dir read AND write, and `$HOME/.config` is
//!    not in `sandbox::system_read_roots()`, so a caged agent could exec a
//!    script in neither. `~/.claude` is already readable in the cage.
//! 2. The script bails unless `TERMIC_TASK_ID` is set and `GROK_HOOK_EVENT` is
//!    not. The install is GLOBAL, so without the first gate we would write OSC
//!    into every terminal the user runs claude in; and Grok reads
//!    `~/.claude/settings.json` too (measured), so without the second we would
//!    silently change an agent the user never opted in for.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Bump when the script body or the settings entry shape changes. Recorded in
/// the manifest so a later version knows an older install is stale and replaces
/// it rather than appending a second entry.
// v2 registered a Working HEARTBEAT (claude/grok PreToolUse, an opencode
// throttle). An install from v1 still works and reports less, so it is stale
// rather than broken.
// v3 writes to the first of three targets that accepts it, so a Docker
// sandboxed agent can reach the terminal at all: $TERMIC_PTY is a HOST path and
// does not exist in the container, which made every hook in a sandboxed tab a
// silent no-op. A v2 install is not stale-but-working there, it is dead, which
// is the strongest reason yet for sync to update installs on its own.
// v4 registers claude's `SessionStart` as `Signal::Ready`. Without it the only
// evidence that an agent can accept typed input is that its terminal painted
// and went quiet, and a blocking startup dialog does exactly that: termic typed
// its first message into claude's "do you trust this folder?" picker and the
// submit landed on the highlighted default, `No, exit`. A v3 install types into
// the dialog exactly as before, so this is stale-and-harmful, not stale-and-
// quieter. See `Signal::Ready`.
// v5 turns claude's Done guard from "is anything in flight" into a whitelist of
// task types. v4 asked the wrong question: an artifact watch is an ambient
// websocket monitor that stays `running` for the whole session, so one publish
// made every later `Stop` look like outstanding work and the tab span forever
// with no demoter left to correct it. A v4 install is not stale-and-quieter, it
// is a tab that never stops loading, which is the same severity as v3 in Docker.
// v6 changes two script BODIES, which an upgrade alone would not pick up: an
// install writes the scripts once and nothing rewrites them afterwards, so a v5
// install keeps reporting exactly what v5 reported. Both changes matter enough
// to be worth a reinstall. claude's and codex's ATTENTION scripts now read
// `tool_name` out of the payload, so the banner names the tool it is blocked on
// rather than saying "agent needs your input"; and codex's READY script now also
// reports the session id, which is the only way a repo-root codex task can
// resume its OWN conversation instead of whichever one ran last in that
// directory.
//
// Safe to bump for an existing codex install: the trust entries in config.toml
// hash the hooks.json ENTRY (command path, timeout, status message), none of
// which this changes, so a reinstall re-asks codex and writes back the same
// hashes rather than orphaning them.
pub const SCHEMA_VERSION: u32 = 6;

/// Directory we create inside the agent's config dir. Also the prefix that
/// identifies our entries for removal, which is why it must never be renamed
/// without a `SCHEMA_VERSION` bump and a migration.
const SCRIPT_DIR: &str = "termic-hooks";
const MANIFEST_NAME: &str = "manifest.json";
const BACKUP_NAME: &str = "config.termic-backup";

/// Where a given install writes. Host is the user's own config dir; Docker is
/// the termic-owned dir that gets bind-mounted into the container, which is why
/// a Docker install mutates nothing of the user's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Carries the agent id, so one call site can serve every agent.
    Host(String),
    /// Carries the agent's OWN id (a clone keeps its own folder), not the base
    /// id. `docker.rs` documents why conflating the two makes clones unusable.
    Docker(String),
}

impl Target {
    /// The agent id this target is for. Docker keys on the agent's OWN id while
    /// the config SHAPE comes from the base id, which is why `command_path`
    /// resolves the base separately.
    pub fn agent(&self) -> &str {
        match self {
            Target::Host(a) | Target::Docker(a) => a,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub schema_version: u32,
    /// Absolute path we wrote into the config. Host or container form.
    pub command: String,
    pub installed_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HookStatus {
    pub installed: bool,
    /// The config file we would write, so the UI can name it BEFORE writing.
    pub settings_path: String,
    pub script_dir: String,
    /// True when the user has set `disableAllHooks`. An install is then a no-op
    /// and the UI must say so rather than report success.
    pub disabled_all: bool,
    /// Set when the config could not be read or parsed. Install is refused.
    pub error: Option<String>,
    pub schema_version: Option<u32>,
    /// Installed, but generated by an older termic whose hook SET differs from
    /// this build's. Surfaced because it is otherwise invisible: the config
    /// looks installed and quietly keeps reporting less than it could. The
    /// heartbeat that stops a cleared spinner staying gone arrived this way,
    /// and without this every existing install would have silently missed it.
    pub stale: bool,
    /// At least one entry of OURS is in the config, i.e. the user opted in at
    /// some point and has not removed it. NOT the same as `installed`, which
    /// demands every event in TODAY's set.
    ///
    /// The distinction is what makes the hook set extensible. `installed` is
    /// an ALL, so the moment a new event joins an agent's set every existing
    /// install fails it - and `agent_hooks_sync` skipping anything not
    /// installed meant adding an event orphaned exactly the installs the sync
    /// exists to upgrade. Caught the first time it mattered: `SessionStart`
    /// was added, agy and grok (whose sets were unchanged) upgraded, and
    /// claude, the agent the event was FOR, silently did not.
    ///
    /// Consent is what this tracks, so it is the right gate for an unattended
    /// upgrade: `remove` deletes every entry of ours, so a user who opted out
    /// reads false here and is never re-installed behind their back.
    pub ours_present: bool,
}

/// What a hook tells termic. Each maps onto an OSC the terminal already
/// understands, so nothing new has to be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Blocked on the user. OSC 777 `notify`, whose TITLE field marks it as
    /// termic's own (see `lib/agentHooks.ts`: a user's `attention` list is an
    /// ALLOW-LIST and would otherwise filter ours out).
    Attention,
    /// Turn started. OSC 133;C, which `TerminalPane` maps to `goWorking`.
    Working,
    /// Turn genuinely over. OSC 133;D, which `TerminalPane` maps to
    /// `goIdle(reason, 0)`: a hard done with no settle wait. It only fires
    /// when we were working, which is why an agent using it needs `Working`
    /// registered too.
    Done,
    /// The session exists and the agent is past its own startup, so a typed
    /// message will reach its input box. OSC 777 `notify` like `Attention`,
    /// distinguished by its BODY (`lib/agentHooks.ts` owns both strings): same
    /// trusted-sender check, no new OSC id to parse, and it survives the Docker
    /// three-target write that `Attention` already relies on.
    ///
    /// This is the one signal termic cannot approximate from the terminal.
    /// Everything else here corrects a state the terminal gets WRONG; this one
    /// reports a state the terminal cannot express at all. A blocking startup
    /// dialog paints and then goes quiet, which is byte-for-byte what a ready
    /// agent looks like, so the quiet heuristic says "ready" and the first
    /// message is typed into a picker whose default answer is destructive.
    Ready,
}

/// OSC 777 `notify` up to and including the sender field. `termic` in the title
/// position is what marks a signal as OURS rather than something the agent chose
/// to say, which is what lets it skip `notificationWantsAttention`'s allow-list
/// (see `lib/agentHooks.ts`).
const NOTIFY_PREFIX: &str = "777;notify;termic;";

/// The generic attention body. Split out from the payload because claude's
/// script composes its body at RUN time and needs this exact string as its
/// fallback: two copies of it would be two things to keep in step.
/// KEEP IN SYNC with `HOOK_OSC_BODY` in `lib/agentHooks.ts`.
const ATTENTION_BODY: &str = "agent needs your input";

/// Prefix of the body that reports the agent's own session id. Only codex sends
/// it, because it is the only agent that can resume a session by id and cannot
/// be handed one at launch. KEEP IN SYNC with `HOOK_OSC_SESSION_PREFIX` in
/// `lib/agentHooks.ts`.
const SESSION_BODY_PREFIX: &str = "session ";

/// KEEP IN SYNC with `HOOK_OSC_READY_BODY` in `lib/agentHooks.ts`. Must never be
/// a prefix of `ATTENTION_BODY` or vice versa: the TS handler routes the two
/// apart on an exact match and would badge a ready session as needing you.
const READY_BODY: &str = "agent ready for input";

impl Signal {
    /// The OSC payload, without introducer or terminator.
    fn payload(self) -> String {
        match self {
            Signal::Attention => format!("{NOTIFY_PREFIX}{ATTENTION_BODY}"),
            Signal::Working => "133;C".into(),
            Signal::Done => "133;D".into(),
            Signal::Ready => format!("{NOTIFY_PREFIX}{READY_BODY}"),
        }
    }
    /// Filename stem for the generated script, so one agent's scripts do not
    /// collide and a reader can tell what each one is for.
    fn stem(self) -> &'static str {
        match self {
            Signal::Attention => "attention",
            Signal::Working => "working",
            Signal::Done => "done",
            Signal::Ready => "ready",
        }
    }

    /// What Claude shows in its own UI while the hook runs. It is USER-VISIBLE,
    /// so it has to describe the signal actually being sent: a single shared
    /// string meant a turn STARTING announced "you are needed", which is a lie
    /// on two of the three hooks. Found by installing into a real config and
    /// reading the result rather than by a test, which is why one now exists.
    fn status_message(self) -> &'static str {
        match self {
            Signal::Attention => "termic: reporting that you are needed",
            Signal::Working => "termic: reporting that this turn started",
            Signal::Done => "termic: reporting that this turn finished",
            Signal::Ready => "termic: reporting that this session is ready",
        }
    }
}

/// Which events an agent gets, and what each one reports. Deliberately minimal:
/// termic already has anything the terminal tells it, so a hook is only
/// registered for a state the terminal gets WRONG or cannot express. Per-tool-
/// call events are never registered, on any agent.
///
/// claude: `PermissionRequest` at +20ms. Its title claims IDLE while blocked,
///   so this is a correction, not an addition. (`Notification` is a +6.0s
///   nudge, measured, so it is the wrong event.)
/// grok: `Notification` (`notificationType=permission_prompt`). grok has no
///   `PermissionRequest`, and its title FREEZES on a busy spinner while
///   blocked, measured at 217s on one frame, so this is the only signal that
///   state has.
/// agy: `Stop` plus `PreInvocation`. agy emits NO OSC whatsoever, so today
///   every turn ends in the byte-quiet fallback's orange bell rather than a
///   done. It has no attention-shaped event at all, so needs-you stays on the
///   fallback. `Working` is required for `Done` to fire, since a hard idle is
///   ignored unless we were working.
pub fn hooks_for(agent: &str) -> &'static [(&'static str, Signal)] {
    match agent {
        // opencode's plugin sees all four edges in-process. It is the only
        // agent that reports permission.replied, so its attention can be
        // cleared exactly rather than waiting for the next busy signal.
        "opencode" => &[
            ("chat.message", Signal::Working),
            ("permission.asked", Signal::Attention),
            ("permission.replied", Signal::Working),
            ("session.idle", Signal::Done),
        ],
        // Working is registered even though the title already reports it,
        // because it makes the pair SELF-SUFFICIENT: `goIdle(reason, 0)` is
        // ignored unless we were working, so a Done that depended on the title
        // having set working would inherit the title's fragility. The Codex
        // latch (see docs/gotchas.md) is what that failure looks like: a vendor
        // changed their title format and done detection silently stopped.
        // `PreToolUse` is a HEARTBEAT, not a duplicate of UserPromptSubmit.
        //
        // Working is a sustained state and every other signal here is an edge.
        // The terminal title, which this replaced, re-asserted working on every
        // repaint, so anything that wrongly cleared the spinner self-healed
        // within a frame. UserPromptSubmit fires ONCE, so the same clear became
        // permanent for the rest of the turn: a user clicking into a running
        // task has its spinner dropped (the manual-clear path) and nothing ever
        // put it back. Reported from a real session, and a straight regression
        // against the title detection it replaced.
        //
        // A tool call is the protocol's own "still going", it lands many times
        // per turn, and an observer hook that exits 0 with no output cannot
        // affect the tool (the same shape as the rtk hook people already run on
        // this event).
        //
        // `SessionStart` fires only once claude is past its own startup, which
        // notably includes the trust picker: "Is this a project you created or
        // one you trust?" with `No, exit` highlighted, and NO hook fires while
        // it is up. Measured. That makes it a true readiness gate rather than a
        // guess, and it is the only signal that distinguishes a blocking dialog
        // from a waiting input box, since both paint and then go quiet.
        //
        // Trust resolves through the REPO, not the directory: a worktree of an
        // already-trusted repo inherits it and gets no record of its own, which
        // is why the picker is not an every-task event. It is the FIRST task in
        // a repo claude has never run in - a project just added to termic, or a
        // machine where claude only ever runs through termic. Also measured,
        // after the opposite was assumed and written down.
        "claude" => &[
            ("SessionStart", Signal::Ready),
            ("UserPromptSubmit", Signal::Working),
            ("PreToolUse", Signal::Working),
            ("PermissionRequest", Signal::Attention),
            ("Stop", Signal::Done),
        ],
        // grok is the only agent measured that reports an INTERRUPT, so it is
        // the only one whose done survives an escape. StopCancelled carries
        // reason=user_interrupt, and also covers a declined permission prompt,
        // --max-turns and a no-progress bail-out.
        "grok" => &[
            ("UserPromptSubmit", Signal::Working),
            ("PreToolUse", Signal::Working),
            ("Notification", Signal::Attention),
            ("Stop", Signal::Done),
            ("StopCancelled", Signal::Done),
        ],
        // agy needs no extra heartbeat: PreInvocation already fires once per
        // model invocation, several times in a turn. Its PreToolUse is also the
        // one tool event across these agents that is NOT safe to observe
        // silently, since `decision` is documented as required, so adding it
        // would risk blocking the tool for no gain.
        "agy" => &[("PreInvocation", Signal::Working), ("Stop", Signal::Done)],
        // codex, and the reason its old "not needed" exclusion is out of date.
        //
        // That exclusion was argued from ATTENTION alone: codex's title says
        // `Action Required` at +22ms, so it needed no hook to report a
        // permission prompt. True, and beside the point for the state that
        // actually broke. Codex is not a hooks target, so it is the ONE agent
        // that can never reach the `hooksOwn` branch, which means every turn it
        // runs is ended by a guess: the byte-quiet fallback calls a turn done
        // after 4s of silence, and 4s of silence is an ordinary model
        // round-trip. That is the GH #276 storm, and codex is the agent it was
        // reported on.
        //
        // The event names are claude's exactly, which is not a coincidence:
        // codex 0.153.0 reads a claude-shaped `hooks.json` and its own
        // `HookEventName` enum lists the same set. Same mapping, same reasons,
        // one difference: codex's hooks do not run until they are TRUSTED (see
        // codex_trust.rs), which is the whole of the extra work here.
        "codex" => &[
            ("SessionStart", Signal::Ready),
            ("UserPromptSubmit", Signal::Working),
            ("PreToolUse", Signal::Working),
            ("PermissionRequest", Signal::Attention),
            ("Stop", Signal::Done),
        ],
        _ => &[],
    }
}

// Why every agent writes to `$TERMIC_PTY`, including claude.
//
// claude CAN return a `terminalSequence` and write the OSC itself, and that was
// the original design. It is not enough. Its runtime allowlists what it will
// write: "only OSC 0/1/2/9/99/777 and BEL are permitted, and OSC 9 bodies may
// not begin with a digit unless in the 9;4 progress form", quoted from the
// binary. OSC 133 is not on that list, so a `Done` sent that way is dropped
// SILENTLY: the hook fires correctly and nothing reaches the parser. Measured.
//
// Staying on `9;4` instead would be allowed but costs the hard done: `9;4;0`
// routes through the 5s settle, while `133;D` is `goIdle(reason, 0)`. For the
// two states that matter most that is the wrong trade, so claude joins
// everyone else on the pty.
//
// The pty path works for every agent because opening a tty BY NAME needs no
// controlling terminal, which is exactly what rules out `/dev/tty` (measured on
// grok: hooks run with no ctty and the write fails, rc=1).

/// Config schema. They are not variations on one shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Schema {
    /// Not a config file at all: a JS module dropped into the plugin
    /// directory. Install is a file write, removal a delete, and there is
    /// nothing of the user's to merge with or preserve.
    OpencodePlugin,
    /// `hooks.<Event>[] = { hooks: [handler] }`. claude and grok both use it,
    /// which is not a coincidence: grok reads claude's file on purpose.
    ClaudeCompatible,
    /// `<name> = { enabled, <Event>: ... }`, and HETEROGENEOUS: tool events take
    /// a `{matcher, hooks}` group while `PreInvocation` / `PostInvocation` /
    /// `Stop` take handlers DIRECTLY. Wrap the latter and they register with an
    /// EMPTY command: visible in `agy -p "/hooks"`, silently inert. Measured.
    AntigravityNamed,
}

fn schema_for(agent: &str) -> Schema {
    match agent {
        "opencode" => Schema::OpencodePlugin,
        "agy" => Schema::AntigravityNamed,
        _ => Schema::ClaudeCompatible,
    }
}

/// How often the opencode plugin may re-assert "working" while a turn streams.
/// Well under any demoter's patience, and far above the raw event rate.
const HEARTBEAT_MS: u32 = 2_000;

/// Top-level key we own in the Antigravity config. Removal deletes exactly this.
const AGY_HOOK_NAME: &str = "termic";

// ─────────────────────────── The script ────────────────────────────────

/// The hook body. `printf '%s'` with a single-quoted argument so the shell
/// never touches the backslashes: the escape and bell reach Claude as the JSON
/// escapes `` / ``, never as raw control bytes, which keeps the file
/// greppable and diffable.
pub fn script_body(agent: &str, sig: Signal) -> String {
    let payload = sig.payload();

    // A Done hook must NOT claim the turn is over while the agent still has
    // work outstanding. claude fires `Stop` with a populated `background_tasks`
    // when it backgrounds a subagent or a shell and keeps waiting (measured),
    // and agy reports the same thing as `fullyIdle: false`. termic used to
    // catch this by scanning the SCREEN for the agent's own status line, which
    // is the kind of text heuristic hooks exist to replace: this reads the
    // protocol instead.
    //
    // The guard is a WHITELIST of task types, and it has to be, because
    // "non-empty" was measured to mean things that never end. Read out of
    // 2.1.259: the Stop payload is built by mapping the whole task registry
    // through one filter, `status is running|pending && isBackgrounded !==
    // false`. Nothing else is dropped, and the switch that decorates the
    // entries has an explicit arm for `monitor_ws` - a websocket monitor.
    // Publishing an artifact opens one (the live-updates subscription) and it
    // stays `running` for the whole session, so from the first publish onward
    // EVERY `Stop` carried a non-empty array and this guard dropped every one
    // of them. claude's own task list hides exactly these
    // (`if (t.type === "monitor_ws" && t.ambient) continue`); the hook payload
    // does not, so termic has to.
    //
    // So the question is not "is anything in flight" but "is the AGENT still
    // working", and only delegated units of work answer yes. The friendly
    // labels come from claude's own type map: local_agent -> subagent,
    // local_workflow -> workflow, local_bash -> shell, in_process_teammate ->
    // teammate, remote_agent -> cloud session, mcp_task -> MCP task. Ambient
    // monitors (monitor_ws / monitor_mcp), `dream` and `auto-mode scan` are
    // deliberately absent, and so is any type a future release adds: an
    // unknown type falls through to done.
    //
    // Fail towards done, not towards working. Once hooks own a tab there is no
    // demoter left to correct a done that never arrives, so a wrong hold is
    // permanent; a wrong done is re-armed by the very next `PreToolUse`
    // heartbeat. The asymmetry runs the opposite way to the fallback path's.
    //
    // Deliberately no `jq`: whitespace is stripped and the field matched
    // literally, so the script keeps its "no dependencies" property. An absent
    // field means an older agent that cannot background work, so done stands.
    // The array is sliced out before matching rather than matched across the
    // whole payload, because `last_assistant_message` carries the agent's own
    // prose and a turn that happened to discuss `"type":"shell"` would
    // otherwise hold its own spinner down forever. `##` (longest prefix) picks
    // the LAST occurrence, which is the real field: claude serialises
    // `last_assistant_message` before `background_tasks`.
    let guard = match (agent, sig) {
        ("claude", Signal::Done) => concat!(
            "flat=$(cat | tr -d '[:space:]')\n",
            "# Only DELEGATED work means the agent itself is still going. An\n",
            "# ambient monitor (an artifact watch) never ends, so it must not\n",
            "# hold the turn open. Unknown types fall through to done.\n",
            "case \"$flat\" in\n",
            "  *'\"background_tasks\":['*)\n",
            "    tasks=${flat##*'\"background_tasks\":['}\n",
            "    tasks=${tasks%%]*}\n",
            "    case \"$tasks\" in\n",
            "      *'\"type\":\"subagent\"'*|*'\"type\":\"workflow\"'*|\\\n",
            "      *'\"type\":\"shell\"'*|*'\"type\":\"teammate\"'*|\\\n",
            "      *'\"type\":\"cloudsession\"'*|*'\"type\":\"MCPtask\"'*) exit 0 ;;\n",
            "    esac\n",
            "    ;;\n",
            "esac\n",
        ),
        ("agy", Signal::Done) => concat!(
            "flat=$(cat | tr -d '[:space:]')\n",
            "# agy states it outright: anything but true means work continues.\n",
            "case \"$flat\" in\n",
            "  *'\"fullyIdle\":false'*) exit 0 ;;\n",
            "esac\n",
        ),
        // Name the tool in the ATTENTION body, because this hook is the signal
        // that actually reaches the user.
        //
        // Measured order (GH #276): the hook fires the instant claude blocks,
        // and claude's own OSC 9 ("Claude needs your permission") arrives a
        // further 6.0s behind it. The banner is therefore always composed from
        // the hook's body, and claude's better wording only ever reaches the
        // badge - it cannot re-notify, and must not, since that would be two
        // banners for one prompt. So the fix is to make the body that WINS the
        // race the informative one, and the hook can be more specific than
        // claude is: it knows which tool is being asked about.
        //
        // `tool_name` is the documented field for the tool events on BOTH
        // agents that get this guard: claude 2.1.259 documents it in its own
        // embedded hooks reference ("session_id", "tool_name", "tool_input"),
        // and codex 0.153.0 requires it in the `permission-request.command.input`
        // JSON Schema its binary carries. Same field, same shape. FIRST
        // occurrence, not last, because that serialisation order puts the real
        // field before `tool_input` - the opposite of the Done guard above,
        // which needs the last one for the opposite reason.
        //
        // Still no jq, same as everything else here. And still exits 0 on
        // every path: an unparseable payload falls back to the generic body
        // rather than emitting nothing.
        // codex reports the id of the session it just started, which is the
        // only way termic can ever resume THAT session rather than "whatever
        // ran last in this directory". Two repo-root tasks share a cwd, so
        // `resume --last` hands the second one the first one's conversation;
        // this is what closes that.
        //
        // `session_id` is required in codex's own `session-start.command.input`
        // schema and was captured from a live 0.153.0 to confirm the position:
        // it is the FIRST field, ahead of `transcript_path` and `cwd`, so the
        // shortest-prefix match takes the real one. Measured across a fresh
        // start AND a resume: the id is the SAME both times (`source` is what
        // differs), so storing it on every spawn is idempotent rather than a
        // chain of ids that has to be kept in order.
        ("codex", Signal::Ready) => concat!(
            "flat=$(cat | tr -d '[:space:]')\n",
            "sid=''\n",
            "case \"$flat\" in\n",
            "  *'\"session_id\":\"'*)\n",
            "    sid=${flat#*'\"session_id\":\"'}\n",
            "    sid=${sid%%'\"'*}\n",
            "    ;;\n",
            "esac\n",
            "# A session id is a UUID and nothing else. It ends up expanded into\n",
            "# a `resume <id>` COMMAND LINE, so anything that is not plainly one\n",
            "# is dropped rather than escaped.\n",
            "case \"$sid\" in\n",
            "  ????????-????-????-????-????????????) ;;\n",
            "  *) sid='' ;;\n",
            "esac\n",
            "case \"$sid\" in\n",
            "  *[!0-9a-fA-F-]*) sid='' ;;\n",
            "esac\n",
        ),
        ("claude", Signal::Attention) | ("codex", Signal::Attention) => concat!(
            "flat=$(cat | tr -d '[:space:]')\n",
            "tool=''\n",
            "case \"$flat\" in\n",
            "  *'\"tool_name\":\"'*)\n",
            "    tool=${flat#*'\"tool_name\":\"'}\n",
            "    tool=${tool%%'\"'*}\n",
            "    ;;\n",
            "esac\n",
            "# A tool name is a bare identifier (Bash, Write, mcp__srv__tool).\n",
            "# Anything else is not one, and a ';' would split the OSC 777\n",
            "# payload into the wrong fields, so reject rather than sanitise.\n",
            "case \"$tool\" in\n",
            "  ''|*[!A-Za-z0-9_-]*) tool='' ;;\n",
            "esac\n",
        ),
        _ => "",
    };

    // Every agent writes straight to the terminal termic handed it. See
    // `uses_terminal_sequence` for why claude does not use its own channel.
    //
    // THREE targets, tried in order, because `$TERMIC_PTY` is a HOST path and
    // a Docker-sandboxed agent cannot see it. Measured inside the real sandbox
    // image: `TERMIC_PTY_EXISTS=NO`, so every hook in a sandboxed tab fired
    // correctly and wrote into nothing. Its neighbours on the same agent,
    // unsandboxed, worked throughout.
    //
    //   $TERMIC_PTY    the slave by name. The host case, and unambiguous.
    //   /proc/1/fd/1   the container's main process stdout, which docker
    //                  relays to that same host pty (`docker run -i -t`).
    //                  Measured to arrive. Needs NO controlling terminal,
    //                  which is the whole reason it beats /dev/tty here.
    //   /dev/tty       last resort. Hooks run with no ctty (measured on grok,
    //                  rc=1), so this usually fails, but it costs one failed
    //                  open and covers a runtime that does give them one.
    //
    // Chained on redirection failure, not on a readiness test: `[ -w /dev/tty ]`
    // is true even where opening it fails, so trying the write IS the test.
    // claude's attention body is composed at RUN time from `$tool` (set by the
    // guard above), so its payload cannot be a compile-time literal like every
    // other one. Two things about the shape are load-bearing:
    //
    //   - the body goes through `%s`, never into printf's FORMAT string. A tool
    //     name is rejected unless it is a bare identifier, but a `%` reaching a
    //     format string would be a bug waiting for the first one that is not.
    //   - the fallback is `HOOK_OSC_BODY` verbatim (`lib/agentHooks.ts`), so a
    //     payload this cannot read behaves exactly as it did before.
    let emit = if (agent, sig) == ("codex", Signal::Ready) {
        // TWO sequences, ONE write. Ready keeps its exact body because the TS
        // side routes it on an exact match; the id rides a second sequence with
        // its own prefix, concatenated into the same `printf`.
        //
        // One write rather than two calls to `emit`, and that is not tidiness:
        // every script here writes with a TRUNCATING redirect, which is
        // meaningless on a pty and total on a regular file. Two writes meant
        // the id erased the ready that preceded it anywhere the target was a
        // file, and ready is the half `seedPrompt` blocks on. Caught by the
        // test below, which writes to a file for exactly that reason.
        //
        // `%s` again, never the format string.
        format!(
            "[ -n \"$TERMIC_PTY\" ] || exit 0\n\
             if [ -n \"$sid\" ]; then\n\
               emit() {{ printf '\\033]{ready}\\007\\033]{NOTIFY_PREFIX}{SESSION_BODY_PREFIX}%s\\007' \"$sid\" > \"$1\" 2>/dev/null; }}\n\
             else\n\
               emit() {{ printf '\\033]{ready}\\007' > \"$1\" 2>/dev/null; }}\n\
             fi\n\
             emit \"$TERMIC_PTY\" || emit /proc/1/fd/1 || emit /dev/tty || true",
            ready = Signal::Ready.payload()
        )
    } else if sig == Signal::Attention && matches!(agent, "claude" | "codex") {
        format!(
            "[ -n \"$TERMIC_PTY\" ] || exit 0\n\
             if [ -n \"$tool\" ]; then\n\
               body=\"needs your permission: $tool\"\n\
             else\n\
               body='{ATTENTION_BODY}'\n\
             fi\n\
             emit() {{ printf '\\033]{NOTIFY_PREFIX}%s\\007' \"$body\" > \"$1\" 2>/dev/null; }}\n\
             emit \"$TERMIC_PTY\" || emit /proc/1/fd/1 || emit /dev/tty || true"
        )
    } else {
        format!(
            "[ -n \"$TERMIC_PTY\" ] || exit 0\n\
             emit() {{ printf '\\033]{payload}\\007' > \"$1\" 2>/dev/null; }}\n\
             emit \"$TERMIC_PTY\" || emit /proc/1/fd/1 || emit /dev/tty || true"
        )
    };

    // grok is the one agent that reads ANOTHER agent's config (it scans
    // ~/.claude/settings.json too), so claude's script must stay silent when
    // grok is the caller or a claude install would silently rewire grok.
    // grok's own script wants the opposite test.
    let provenance = if agent == "grok" {
        "# Only meaningful when grok is the caller; this file is grok's own.\n[ -n \"$GROK_HOOK_EVENT\" ] || exit 0"
    } else if agent == "claude" {
        "# grok reads ~/.claude/settings.json too, so this file also runs under\n# grok. The user never opted grok in here, and grok has its own install.\n[ -z \"$GROK_HOOK_EVENT\" ] || exit 0"
    } else {
        "# This file is only read by its own agent."
    };

    let what = sig.stem();
    format!(
        r#"#!/bin/sh
# termic agent hook for {agent} ({what}, schema v{SCHEMA_VERSION}). Safe to delete.
#
# Puts one OSC sequence on the agent's own terminal so termic knows its state.
# No network, no files, no arguments, no stdin parsing.
# Exits 0 on every path: a hook must never be why an agent stalls.

# Not spawned by a termic PTY (this file is installed globally, so it also runs
# in iTerm, Ghostty and CI). Stay silent there.
[ -n "$TERMIC_TASK_ID" ] || exit 0

{provenance}

{guard}{emit}
exit 0
"#
    )
}

/// opencode's plugin, which is a JS module rather than a spawned hook.
///
/// It runs IN-PROCESS inside opencode, so the safety model inverts: there is no
/// timeout and no exit code, and a throw inside `tool.execute.before` blocks
/// the tool (opencode's own documented example for it). Every handler is
/// therefore individually wrapped, and the module does nothing at all outside a
/// termic pty.
///
/// Being in-process is also why it writes with `fs` rather than spawning
/// anything: no process per event.
fn opencode_plugin_body() -> String {
    let attention = Signal::Attention.payload();
    let working = Signal::Working.payload();
    let done = Signal::Done.payload();
    format!(
        r#"// termic agent hook for opencode (generated, schema v{SCHEMA_VERSION}). Safe to delete.
//
// Reports opencode's state to termic by writing one OSC sequence to the
// terminal termic handed it ($TERMIC_PTY). opencode emits no busy/idle OSC of
// its own, so without this every turn ends in termic's byte-quiet fallback.
//
// Runs IN-PROCESS: no timeout, no exit code, and a throw in a tool handler
// blocks the tool. Every handler below is wrapped for that reason.
import {{ writeFileSync }} from "fs";

const PTY = process.env.TERMIC_PTY;
// Not spawned by a termic pty (this file is installed globally, so it also
// loads under a plain `opencode` in any terminal). Do nothing there.
const ACTIVE = Boolean(PTY && process.env.TERMIC_TASK_ID);

// Same three targets as the shell hooks, same reason: $TERMIC_PTY is a HOST
// path, so a Docker-sandboxed opencode cannot see it. /proc/1/fd/1 is the
// container's main process stdout, which docker relays to that same pty.
const TARGETS = [PTY, "/proc/1/fd/1", "/dev/tty"];

const send = (payload) => {{
  if (!ACTIVE) return;
  for (const t of TARGETS) {{
    if (!t) continue;
    try {{ writeFileSync(t, `\x1b]${{payload}}\x07`); return; }} catch {{ /* try the next */ }}
  }}
}};

const ATTENTION = "{attention}";
const WORKING   = "{working}";
const DONE      = "{done}";

// Heartbeat. `chat.message` fires ONCE per turn, and working is a sustained
// state: anything that clears the spinner mid-turn (the user clicking into the
// task drops it) would otherwise never be undone, which is the regression the
// terminal title did not have, because it re-asserted on every repaint.
// opencode streams `message.part.delta` continuously while it works (measured:
// hundreds per turn), so re-asserting on those restores self-healing. Throttled
// because the raw rate is far too high to write an OSC per event.
let lastBeat = 0;
const beat = () => {{
  const now = Date.now();
  if (now - lastBeat < {HEARTBEAT_MS}) return;
  lastBeat = now;
  send(WORKING);
}};

export const TermicStatus = async () => ({{
  // One per turn, on submit.
  "chat.message": async () => {{ try {{ send(WORKING); lastBeat = Date.now(); }} catch {{}} }},
  event: async ({{ event }}) => {{
    try {{
      if (event?.type === "message.part.delta" || event?.type === "message.part.updated") {{
        beat();
        return;
      }}
      switch (event?.type) {{
        // The only agent measured that reports the block AND its release, so
        // attention here can be cleared exactly rather than inferred.
        case "permission.asked":   send(ATTENTION); break;
        case "permission.replied": send(WORKING);   break;
        case "session.idle":       send(DONE);      break;
      }}
    }} catch {{ /* never throw into opencode */ }}
  }},
}});
"#
    )
}

// ───────────────────────── Pure JSON surgery ───────────────────────────
//
// Kept pure and separate from the filesystem so the merge rules can be tested
// exhaustively without a HOME fixture. These are the functions that must not
// eat a user's hand-written hooks.

/// True when this hook entry is one of ours, decided by the `command` path
/// prefix rather than a marker key. A marker would need Claude's schema to
/// tolerate unknown fields (Codex's rejects the whole file over one), and a
/// path survives the user reformatting their config.
fn is_ours(entry: &Value, prefix: &str) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.starts_with(prefix))
}

/// Strip every entry of ours from `hooks.<EVENT>`, dropping groups and keys
/// that become empty. Returns true when anything was removed.
fn strip_ours(root: &mut Value, prefix: &str, event: &str) -> bool {
    let mut removed = false;
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    if let Some(groups) = hooks.get_mut(event).and_then(Value::as_array_mut) {
        for group in groups.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = list.len();
                list.retain(|e| !is_ours(e, prefix));
                removed |= list.len() != before;
            }
        }
        // A group whose hook list we emptied was ours alone; drop it. A group
        // that still holds user hooks stays exactly as it was.
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|l| !l.is_empty())
        });
        if groups.is_empty() {
            hooks.remove(event);
        }
    }
    if hooks.is_empty() {
        root.as_object_mut().map(|o| o.remove("hooks"));
    }
    removed
}

/// Insert our entry, replacing any older one of ours. Every unknown key at
/// every level is preserved: we only ever touch `hooks.<EVENT>`.
pub fn merge(existing: &Value, command: &str, prefix: &str, event: &str, status: &str) -> Value {
    let mut root = if existing.is_object() {
        existing.clone()
    } else {
        Value::Object(Map::new())
    };
    strip_ours(&mut root, prefix, event);

    let entry = serde_json::json!({
        "type": "command",
        "command": command,
        "timeout": 5,
        "statusMessage": status,
    });

    let obj = root.as_object_mut().expect("root is an object");
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    let hooks = hooks.as_object_mut().expect("hooks is an object");
    let groups = hooks.entry(event).or_insert_with(|| Value::Array(vec![]));
    if !groups.is_array() {
        *groups = Value::Array(vec![]);
    }
    groups
        .as_array_mut()
        .expect("groups is an array")
        .push(serde_json::json!({ "hooks": [entry] }));
    root
}

/// Remove our entry. Returns `None` when there was nothing of ours to remove,
/// so the caller can leave the file completely untouched.
pub fn unmerge(existing: &Value, prefix: &str, event: &str) -> Option<Value> {
    let mut root = existing.clone();
    if !strip_ours(&mut root, prefix, event) {
        return None;
    }
    Some(root)
}

/// Antigravity's config, which is a different shape and needs its own pair.
/// One named entry we own outright, so install is a set and removal a delete.
///
/// The heterogeneity is the trap: tool events take a `{matcher, hooks}` group
/// while `PreInvocation` / `PostInvocation` / `Stop` take handlers DIRECTLY.
/// Wrapping the latter registers them with an EMPTY command, which `agy -p
/// "/hooks"` will show and which fires nothing. Measured.
fn agy_entry(commands: &[(&str, String, Signal)]) -> Value {
    let mut obj = Map::new();
    obj.insert("enabled".into(), Value::Bool(true));
    for (event, command, _sig) in commands {
        let handler = serde_json::json!({
            "type": "command", "command": command, "timeout": 5,
        });
        let is_tool_event = matches!(*event, "PreToolUse" | "PostToolUse");
        let v = if is_tool_event {
            serde_json::json!([{ "matcher": "*", "hooks": [handler] }])
        } else {
            serde_json::json!([handler])
        };
        obj.insert((*event).to_string(), v);
    }
    Value::Object(obj)
}

fn agy_merge(existing: &Value, commands: &[(&str, String, Signal)]) -> Value {
    let mut root = if existing.is_object() { existing.clone() } else { Value::Object(Map::new()) };
    root.as_object_mut()
        .expect("root is an object")
        .insert(AGY_HOOK_NAME.into(), agy_entry(commands));
    root
}

fn agy_unmerge(existing: &Value) -> Option<Value> {
    let mut root = existing.clone();
    let removed = root.as_object_mut()?.remove(AGY_HOOK_NAME).is_some();
    if removed { Some(root) } else { None }
}

/// Whether the user has switched every hook off. Install must respect it and
/// say so, rather than writing a file that will never fire.
pub fn disable_all_hooks(root: &Value) -> bool {
    root.get("disableAllHooks")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

// ─────────────────────────── Paths ─────────────────────────────────────

/// The agent's own config dir on the host, from the same table Seatbelt and
/// Docker read so the three cannot drift.
///
/// The e2e build lets `TERMIC_E2E_AGENT_HOME` stand in for `$HOME`. Without it
/// the suite's only way to exercise install/remove would be to write into the
/// developer's REAL config, which is not a test, it is a hazard. Feature-gated
/// so no release binary can be pointed anywhere but the user's own home.
/// Indexed in `docs/tech-debt.md`.
fn agent_home() -> Result<PathBuf, String> {
    #[cfg(feature = "e2e")]
    {
        if let Some(v) = std::env::var_os("TERMIC_E2E_AGENT_HOME") {
            return Ok(PathBuf::from(v));
        }
    }
    dirs::home_dir().ok_or_else(|| "no home directory".to_string())
}

/// Home-relative directory holding the agent's config.
fn state_dir(agent: &str) -> Result<&'static str, String> {
    crate::agent_dirs::state_dirs(agent)
        .first()
        .copied()
        .ok_or_else(|| format!("{agent} has no known state dir"))
}

/// Which BUILT-IN an agent behaves as: itself, or what it was cloned from.
///
/// Everything about a hook comes from this (the event set, the config schema,
/// the file within the config dir), because a clone of claude runs the claude
/// binary and reads claude's config shape. Keyed on the base, a duplicated
/// agent was simply unsupported: `SUPPORTED` never matched it, so the row read
/// "could not resolve the agent config directory" and the user got no hooks on
/// the account they made the clone for.
///
/// Deliberately NOT `docker::base_agent_id_str`, which falls back to "claude"
/// for an unrecognised base. That is right for deciding a Docker mount and
/// wrong here: it would install claude's hooks into an unrelated agent's
/// config. An unknown agent stays unknown and is reported unsupported.
fn base_of(agent_id: &str) -> String {
    let agents = crate::load_settings_inner().agents;
    crate::docker::base_agent_id(&agents, agent_id).to_string()
}

/// The directory we write into, on the host filesystem, for a given target.
///
/// Resolved from the agent ENTRY, not from its base's default dir. A clone made
/// to hold a second account relocates its whole config with the agent's own env
/// var, and writing the base's path here would put one account's hooks into the
/// other account's config, which is worse than installing none.
pub fn config_dir(target: &Target) -> Result<PathBuf, String> {
    match target {
        Target::Host(agent) => {
            let home = agent_home()?;
            let agents = crate::load_settings_inner().agents;
            crate::agent_dirs::instance_config_dir(&agents, agent, &home)
                .ok_or_else(|| format!("{agent} has no known state dir"))
        }
        Target::Docker(agent_id) => Ok(crate::docker::agent_config_host_dir(agent_id)),
    }
}

/// The config FILE we merge into, which is not the same shape per agent:
/// claude keeps hooks in `settings.json` alongside everything else, grok reads
/// every `*.json` under `hooks/` so it gets a file of its own (which also makes
/// removal a delete rather than a merge-back).
fn settings_rel(agent: &str) -> &'static str {
    match agent {
        // The plugin itself. `.opencode/plugin` AND `.opencode/plugins` are
        // BOTH loaded (measured: writing both double-fires every event), so
        // only ever the documented plural.
        "opencode" => "plugins/termic.js",
        // grok reads every *.json under hooks/, so it gets a file of its own
        // and removal is a delete rather than a merge-back.
        "grok" => "hooks/termic.json",
        // Antigravity's LIVE path. `~/.gemini/antigravity-cli/hooks.json` also
        // parses and logs "loaded 1 named hooks", and then executes nothing:
        // their own changelog records that path as a bug they fixed because it
        // was desynchronised from the backend. Measured; do not "simplify" this
        // to the other one.
        "agy" => "config/hooks.json",
        // codex keeps hooks in their own file, NOT in config.toml: measured
        // via its `hooks/list`, which reports a hook written here with
        // `source: "user"`. config.toml is still touched, but only for the
        // TRUST entry (codex_trust.rs), never for the hooks themselves.
        "codex" => "hooks.json",
        _ => "settings.json",
    }
}

/// Directory prefix every script of ours lives under, as the CONFIG should
/// name it. Everything under this is ours, which is how removal identifies our
/// entries without needing a marker key (Codex's schema rejects a whole file
/// over one unknown key, and a path survives the user reformatting the config).
///
/// For Docker this must be the path as the CONTAINER sees it, not the host
/// path: the config dir is bind-mounted at `CONTAINER_HOME`, so a host path
/// would not resolve inside the cage.
pub fn command_prefix(target: &Target) -> Result<String, String> {
    Ok(match target {
        Target::Host(_) => format!(
            "{}/",
            config_dir(target)?.join(SCRIPT_DIR).to_string_lossy()
        ),
        Target::Docker(agent_id) => format!(
            "{}/{}/{}/",
            crate::docker::CONTAINER_HOME,
            state_dir(crate::docker::base_agent_id_str(agent_id))?,
            SCRIPT_DIR
        ),
    })
}

fn settings_path(target: &Target) -> Result<PathBuf, String> {
    Ok(config_dir(target)?.join(settings_rel(&base_of(target.agent()))))
}

fn script_dir(target: &Target) -> Result<PathBuf, String> {
    Ok(config_dir(target)?.join(SCRIPT_DIR))
}

// ─────────────────────────── Filesystem ────────────────────────────────

/// NOTE: `serde_json` is built with `preserve_order` (see `Cargo.toml`).
/// Without it `Map` is a `BTreeMap` and every install silently re-sorts the
/// user's `settings.json` into alphabetical order, which is a visible,
/// pointless rewrite of a file they hand-wrote, and it also defeats the
/// byte-identical restore below.
///
/// Write via a temp file in the SAME directory then rename, so a crash or a
/// full disk can never leave a half-written `settings.json` behind. That file
/// breaks the user's agent, not just termic.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or_else(|| "no parent dir".to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(
        ".{}.termic-tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| format!("create temp: {e}"))?;
        f.write_all(bytes).map_err(|e| format!("write temp: {e}"))?;
        f.sync_all().map_err(|e| format!("sync temp: {e}"))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("rename into place: {e}"))
}

/// Read and parse settings.json. A missing file is an empty object; malformed
/// JSON is an ERROR, never an empty object, because overwriting a config we
/// failed to understand would destroy the user's own hooks.
fn read_settings(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(e) => Err(format!("read {}: {e}", path.display())),
        Ok(s) if s.trim().is_empty() => Ok(Value::Object(Map::new())),
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| format!("{} is not valid JSON: {e}", path.display())),
    }
}

pub fn status(target: &Target) -> HookStatus {
    // The BASE: what this agent behaves as. Paths still come from the target,
    // which carries the instance id, so a clone writes into its own config dir.
    let agent = base_of(target.agent());
    let settings = settings_path(target);
    let script = script_dir(target);
    let (settings_path_s, script_dir_s) = (
        settings.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        script.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
    );
    let mut out = HookStatus {
        installed: false,
        settings_path: settings_path_s,
        script_dir: script_dir_s,
        disabled_all: false,
        error: None,
        schema_version: None,
        stale: false,
        ours_present: false,
    };
    let (Ok(settings), Ok(script)) = (settings, script) else {
        out.error = Some("could not resolve the agent config directory".into());
        return out;
    };
    if schema_for(&agent) == Schema::OpencodePlugin {
        // A JS file, not JSON: presence IS the install, and there are no
        // per-event entries, so consent and completeness coincide.
        out.installed = settings.exists();
        out.ours_present = out.installed;
        out.schema_version = std::fs::read_to_string(script.join(MANIFEST_NAME))
            .ok()
            .and_then(|s| serde_json::from_str::<Manifest>(&s).ok())
            .map(|m| m.schema_version);
        out.stale = out.ours_present && out.schema_version != Some(SCHEMA_VERSION);
        return out;
    }
    let root = match read_settings(&settings) {
        Ok(v) => v,
        Err(e) => { out.error = Some(e); return out; }
    };
    out.disabled_all = disable_all_hooks(&root);
    let Ok(prefix) = command_prefix(target) else { return out };

    match schema_for(&agent) {
        Schema::ClaudeCompatible => {
            let hooks = hooks_for(&agent);
            let has = |event: &str| {
                root.get("hooks")
                    .and_then(|h| h.get(event))
                    .and_then(Value::as_array)
                    .is_some_and(|groups| groups.iter().any(|g| {
                        g.get("hooks").and_then(Value::as_array)
                            .is_some_and(|l| l.iter().any(|e| is_ours(e, &prefix)))
                    }))
            };
            // Every registered event must be present, or a partial install
            // would report as done and quietly miss a signal.
            out.installed = !hooks.is_empty() && hooks.iter().all(|(event, _)| has(event));
            // codex only: hooks it does not TRUST are discovered, reported
            // enabled, and never run (codex_trust.rs). Reporting "on" for those
            // would be the UI stating the opposite of the truth, so the trust
            // entry is part of what "installed" means here. Read from
            // config.toml rather than asked of codex: status runs for every
            // agent on every Settings mount and cannot spawn a process per row.
            if agent == "codex" && out.installed && matches!(target, Target::Host(_)) {
                let cfg = config_dir(target)
                    .ok()
                    .and_then(|d| std::fs::read_to_string(d.join("config.toml")).ok())
                    .unwrap_or_default();
                if !crate::codex_trust::is_trusted_here(&cfg, &settings) {
                    out.installed = false;
                    out.error = Some(
                        "codex has the hooks but has not been told to trust them, so they \
                         will not run. Re-install them to fix it."
                            .into(),
                    );
                }
            }
            // ANY entry is consent. A set that gained an event since this
            // install was written is incomplete, not absent, and it is the
            // case sync exists for.
            out.ours_present = hooks.iter().any(|(event, _)| has(event));
        }
        Schema::AntigravityNamed => {
            out.installed = root.get(AGY_HOOK_NAME).is_some();
            // One key we own outright: it is there or it is not.
            out.ours_present = out.installed;
        }
        // Handled by the early return above: the plugin is one file we write
        // whole, so there is no config to inspect or merge. Spelled out rather
        // than a catch-all so a NEW schema still fails to compile here.
        Schema::OpencodePlugin => unreachable!("opencode returns before this"),
    };
    out.schema_version = std::fs::read_to_string(script.join(MANIFEST_NAME))
        .ok()
        .and_then(|s| serde_json::from_str::<Manifest>(&s).ok())
        .map(|m| m.schema_version);
    // An install predating the manifest has no version at all, which is older
    // than anything and therefore stale too. Keyed on `ours_present`, not
    // `installed`: an install missing an event ADDED since it was written is
    // the definition of stale, and keying on the stricter flag made exactly
    // that case invisible to sync.
    out.stale = out.ours_present && out.schema_version != Some(SCHEMA_VERSION);
    out
}

/// Trace line for the trust step. It is the one part of an install that can
/// fail for a reason outside termic (codex missing, not on PATH, an app-server
/// that will not answer), so it says so in the same log every other work-state
/// decision lands in rather than only in a returned error the UI may collapse.
fn log_trust(msg: &str) {
    crate::dlog(&format!("[agent-hooks] {msg}"));
}

/// Where codex keeps ITS config for this target, which is the same dir the
/// hooks file lives in. `CODEX_HOME` is how a clone points codex at a second
/// account, and `config_dir` already resolves that, so the trust entry follows
/// the hooks into whichever home they were written to.
fn codex_home_for(target: &Target) -> Result<PathBuf, String> {
    config_dir(target)
}

/// The codex binary to ask. Resolved from the REGISTRY entry rather than
/// hard-coded, so a user who renamed the command or pointed it at an absolute
/// path gets their binary asked, not a `codex` that may not exist.
fn codex_binary(target: &Target) -> String {
    let agents = crate::load_settings_inner().agents;
    crate::agent_dirs::resolve_agent(&agents, target.agent())
        .map(|a| a.command)
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| "codex".to_string())
}

/// Ask codex for the hash of each hook we just wrote, then record it as
/// trusted. Fails the install when it cannot: a codex install that silently
/// ends with untrusted hooks looks identical to a working one and reports
/// nothing, which is the failure this whole feature exists to remove.
fn trust_codex_hooks(target: &Target, settings: &Path, prefix: &str) -> Result<(), String> {
    let home = codex_home_for(target)?;
    let bin = codex_binary(target);
    // cwd only scopes which PROJECT-level hooks codex reports; ours are
    // user-level and are listed for any cwd. The home dir is a directory that
    // always exists and can never be a git repo with its own `.codex`.
    let found = crate::codex_trust::discover_ours(&bin, &home, settings, prefix, &home)?;
    if found.is_empty() {
        return Err(format!(
            "codex did not report the hooks termic just wrote to {}. They would \
             be installed but never run.",
            settings.display()
        ));
    }
    let cfg = home.join("config.toml");
    let existing = match std::fs::read_to_string(&cfg) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("read {}: {e}", cfg.display())),
    };
    let next = crate::codex_trust::with_trust(&existing, &found)?;
    write_atomic(&cfg, next.as_bytes())?;
    log_trust(&format!("codex: trusted {} hook(s) in {}", found.len(), cfg.display()));
    Ok(())
}

/// Remove the trust entries for the hooks file we are uninstalling.
fn untrust_codex_hooks(target: &Target, settings: &Path) -> Result<(), String> {
    if matches!(target, Target::Docker(_)) {
        return Ok(());
    }
    let cfg = codex_home_for(target)?.join("config.toml");
    let existing = match std::fs::read_to_string(&cfg) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("read {}: {e}", cfg.display())),
    };
    let next = crate::codex_trust::without_trust(&existing, settings)?;
    if next != existing {
        write_atomic(&cfg, next.as_bytes())?;
    }
    Ok(())
}

/// One script per (agent, signal). A bare absolute path with no arguments, so
/// there is no quoting hazard inside JSON-inside-config on any agent.
fn script_for(dir: &Path, sig: Signal) -> PathBuf {
    dir.join(format!("{}.sh", sig.stem()))
}

/// Same, as the path the CONFIG should name. Docker needs the container view.
fn command_for(target: &Target, sig: Signal) -> Result<String, String> {
    Ok(format!("{}{}.sh", command_prefix(target)?, sig.stem()))
}

pub fn install(target: &Target) -> Result<(), String> {
    // The BASE: what this agent behaves as. Paths still come from the target,
    // which carries the instance id, so a clone writes into its own config dir.
    let agent = base_of(target.agent());
    // codex in Docker: write NOTHING, and succeed.
    //
    // Its hooks do not run until a trust entry names them by the path codex
    // RESOLVED plus a hash only codex can produce (codex_trust.rs). Inside a
    // container both are unknowable from out here: the path is the container's
    // (`/root/.codex/hooks.json`, not the host dir termic wrote), and the
    // app-server that would report the hash runs in a container that does not
    // exist yet at install time.
    //
    // So the choice is between writing hooks that are discovered and silently
    // never run, and writing none. None is the honest one: an install that
    // reports success while the agent reports nothing is precisely the failure
    // this feature exists to remove, and `status()` then says "off" for the
    // Docker half, which is true. Returning Ok rather than Err matters as much:
    // `agent_hooks_install` rolls the HOST install back on a Docker error, so
    // refusing here would make codex hooks uninstallable everywhere.
    if agent == "codex" && matches!(target, Target::Docker(_)) {
        log_trust("codex: skipping the Docker half, its hooks cannot be trusted from outside the container");
        return Ok(());
    }
    let hooks = hooks_for(&agent);
    if hooks.is_empty() {
        return Err(format!("hooks are not supported for {agent} yet"));
    }
    let settings = settings_path(target)?;
    let dir = script_dir(target)?;

    // Refuse rather than clobber a config we could not parse.
    let root = read_settings(&settings)?;
    if disable_all_hooks(&root) {
        return Err(
            "disableAllHooks is set in this config, so a hook would never run. \
             Clear it first."
                .into(),
        );
    }

    // Back the original up once, before the first write, so a botched merge is
    // recoverable and removal can restore byte-for-byte.
    let backup = dir.join(BACKUP_NAME);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    if !backup.exists() && settings.exists() {
        std::fs::copy(&settings, &backup).map_err(|e| format!("back up the config: {e}"))?;
    }

    // opencode is a single JS module, not a set of scripts plus a config
    // merge. Nothing of the user's is touched, so there is nothing to back up
    // or merge back.
    if schema_for(&agent) == Schema::OpencodePlugin {
        std::fs::create_dir_all(settings.parent().ok_or("no plugin dir")?)
            .map_err(|e| format!("create plugin dir: {e}"))?;
        write_atomic(&settings, opencode_plugin_body().as_bytes())?;
        let manifest = Manifest {
            schema_version: SCHEMA_VERSION,
            command: settings.to_string_lossy().into_owned(),
            installed_at: chrono::Utc::now().to_rfc3339(),
        };
        return write_atomic(
            &dir.join(MANIFEST_NAME),
            serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?.as_bytes(),
        );
    }

    let mut commands: Vec<(&str, String, Signal)> = Vec::new();
    for (event, sig) in hooks {
        let script = script_for(&dir, *sig);
        write_atomic(&script, script_body(&agent, *sig).as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("chmod hook script: {e}"))?;
        }
        commands.push((event, command_for(target, *sig)?, *sig));
    }

    let prefix = command_prefix(target)?;
    let merged = match schema_for(&agent) {
        Schema::ClaudeCompatible => {
            let mut acc = root;
            for (event, command, sig) in &commands {
                acc = merge(&acc, command, &prefix, event, sig.status_message());
            }
            acc
        }
        Schema::AntigravityNamed => agy_merge(&root, &commands),
        // Handled by the early return above: the plugin is one file we write
        // whole, so there is no config to inspect or merge. Spelled out rather
        // than a catch-all so a NEW schema still fails to compile here.
        Schema::OpencodePlugin => unreachable!("opencode returns before this"),
    };
    let mut bytes = serde_json::to_vec_pretty(&merged).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    write_atomic(&settings, &bytes)?;

    // codex only, and it is not optional: its hooks are discovered, reported
    // `enabled: true`, and then NOT RUN until they are trusted. Everything
    // above would leave a hook that fires nothing and says nothing about why.
    // Deliberately AFTER the write, because the hash codex reports covers the
    // hook as written. See codex_trust.rs.
    if agent == "codex" {
        trust_codex_hooks(target, &settings, &prefix)?;
    }

    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        command: commands.first().map(|(_, c, _)| c.clone()).unwrap_or_default(),
        installed_at: chrono::Utc::now().to_rfc3339(),
    };
    write_atomic(
        &dir.join(MANIFEST_NAME),
        serde_json::to_string_pretty(&manifest)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )
}

pub fn remove(target: &Target) -> Result<(), String> {
    // The BASE: what this agent behaves as. Paths still come from the target,
    // which carries the instance id, so a clone writes into its own config dir.
    let agent = base_of(target.agent());
    let settings = settings_path(target)?;
    let dir = script_dir(target)?;
    let prefix = command_prefix(target)?;

    // Drop codex's trust entries FIRST, and never fail the uninstall over them.
    // They name a file that is about to stop containing our hooks, so leaving
    // them behind is dead config pointing at nothing; but a user who removes
    // hooks wants them gone, and refusing that because a config.toml could not
    // be parsed would trap them. No codex call is needed here (the key begins
    // with the hooks path), so the only way this fails is an unreadable config,
    // which is the user's to fix and ours to leave alone.
    if agent == "codex" {
        if let Err(e) = untrust_codex_hooks(target, &settings) {
            log_trust(&format!("codex untrust skipped: {e}"));
        }
    }

    // Deleting a file we wrote whole. Nothing to unmerge.
    if schema_for(&agent) == Schema::OpencodePlugin {
        if settings.exists() {
            std::fs::remove_file(&settings)
                .map_err(|e| format!("remove {}: {e}", settings.display()))?;
        }
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("remove {}: {e}", dir.display()))?;
        }
        return Ok(());
    }

    let root = read_settings(&settings)?;
    let stripped = match schema_for(&agent) {
        Schema::ClaudeCompatible => {
            let mut acc = root.clone();
            let mut touched = false;
            for (event, _) in hooks_for(&agent) {
                if let Some(next) = unmerge(&acc, &prefix, event) {
                    acc = next;
                    touched = true;
                }
            }
            if touched { Some(acc) } else { None }
        }
        Schema::AntigravityNamed => agy_unmerge(&root),
        // Handled by the early return above: the plugin is one file we write
        // whole, so there is no config to inspect or merge. Spelled out rather
        // than a catch-all so a NEW schema still fails to compile here.
        Schema::OpencodePlugin => unreachable!("opencode returns before this"),
    };

    if let Some(stripped) = stripped {
        // If what remains matches the pre-install backup, restore the backup's
        // BYTES: that is the only way "removal leaves the file byte-identical"
        // survives our own pretty-printer reformatting the user's spacing.
        let backup = dir.join(BACKUP_NAME);
        let restored = std::fs::read(&backup).ok().filter(|b| {
            serde_json::from_slice::<Value>(b).is_ok_and(|orig| orig == stripped)
        });
        match restored {
            Some(bytes) => write_atomic(&settings, &bytes)?,
            None => {
                let mut bytes = serde_json::to_vec_pretty(&stripped).map_err(|e| e.to_string())?;
                bytes.push(b'\n');
                write_atomic(&settings, &bytes)?;
            }
        }
    }
    // Remove our directory whether or not the config entry was there: a user
    // who hand-deleted the entry still wants the scripts gone. grok's config is
    // a file we own outright, so that goes too.
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("remove {}: {e}", dir.display()))?;
    }
    if settings_rel(&agent).starts_with("hooks/") && settings.exists() {
        std::fs::remove_file(&settings).map_err(|e| format!("remove {}: {e}", settings.display()))?;
    }
    Ok(())
}

// ─────────────────────────── Commands ──────────────────────────────────

/// Agents this build can wire. Phase 1 is claude alone; the UI reads this so a
/// row can say "not supported yet" rather than offering a button that fails.
/// Agents this build can wire. Each needs a measured event AND a transport
/// that reaches termic; see `event_for` / `uses_terminal_sequence`.
pub const SUPPORTED: &[&str] = &["claude", "grok", "agy", "opencode", "codex"];

fn check_supported(agent_id: &str) -> Result<(), String> {
    // A duplicated agent is supported when what it was cloned FROM is. It runs
    // the same binary and reads the same config shape, and the only reason it
    // was rejected before is that this list holds built-in names.
    let base = base_of(agent_id);
    if SUPPORTED.contains(&base.as_str()) {
        Ok(())
    } else {
        Err(format!("hooks are not supported for {agent_id} yet"))
    }
}

/// Per-agent status across BOTH targets. One toggle governs the pair, so the UI
/// needs to see both to render a single honest row.
#[derive(Debug, Clone, Serialize)]
pub struct AgentHookStatus {
    pub agent_id: String,
    pub supported: bool,
    pub host: HookStatus,
    pub docker: HookStatus,
}

/// Everything an install would write, for an audience that will read it.
/// These users run coding agents for a living: the honest thing is to show the
/// exact files and the exact script contents BEFORE touching anything, not a
/// reassuring sentence.
#[derive(Debug, Clone, Serialize)]
pub struct HookPlanEntry {
    /// The agent's own event name, e.g. `PermissionRequest`.
    pub event: String,
    /// What termic learns from it: attention, working or done.
    pub reports: String,
    /// Absolute path of the script this event runs.
    pub script_path: String,
    /// The script, verbatim. Short by design, precisely so it can be read.
    pub script_body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HookPlan {
    pub agent_id: String,
    pub supported: bool,
    /// The config file termic edits, and whether it is shared with the user's
    /// own settings or a file termic owns outright.
    pub config_path: String,
    pub config_is_shared: bool,
    /// The exact JSON fragment merged into that config.
    pub config_fragment: String,
    pub entries: Vec<HookPlanEntry>,
    /// Anything else the install changes about how the agent runs.
    pub notes: Vec<String>,
}

#[tauri::command]
pub fn agent_hooks_plan(agent_id: String) -> Result<HookPlan, String> {
    let target = Target::Host(agent_id.clone());
    let hooks = hooks_for(&agent_id);
    let prefix = command_prefix(&target).unwrap_or_default();
    let entries: Vec<HookPlanEntry> = hooks
        .iter()
        .map(|(event, sig)| HookPlanEntry {
            event: (*event).to_string(),
            reports: sig.stem().to_string(),
            script_path: format!("{prefix}{}.sh", sig.stem()),
            script_body: script_body(&agent_id, *sig),
        })
        .collect();

    let shared = settings_rel(&agent_id) == "settings.json";
    let fragment = if hooks.is_empty() {
        String::new()
    } else {
        let commands: Vec<(&str, String, Signal)> = hooks
            .iter()
            .map(|(e, s)| (*e, format!("{prefix}{}.sh", s.stem()), *s))
            .collect();
        let merged = match schema_for(&agent_id) {
            Schema::AntigravityNamed => agy_merge(&Value::Object(Map::new()), &commands),
            Schema::OpencodePlugin => Value::String("(a JS plugin file, shown below)".into()),
            Schema::ClaudeCompatible => {
                let mut acc = Value::Object(Map::new());
                for (event, command, sig) in &commands {
                    acc = merge(&acc, command, &prefix, event, sig.status_message());
                }
                acc
            }
        };
        serde_json::to_string_pretty(&merged).unwrap_or_default()
    };

    let mut notes = Vec::new();
    if !hooks.is_empty() {
        notes.push(
            "Each script writes one OSC sequence to this terminal and exits 0. \
             No network, no file writes, no arguments."
                .into(),
        );
        notes.push(
            "They stay silent unless TERMIC_TASK_ID is set, so the same files do \
             nothing when you run the agent in another terminal."
                .into(),
        );
    }
    if shared && !hooks.is_empty() {
        notes.push(
            "This file is yours and may already contain your own hooks. termic \
             appends one entry per event and removes only those, and it keeps a \
             backup taken before the first install."
                .into(),
        );
    }
    if agent_id == "claude" {
        notes.push(
            "grok also reads ~/.claude/settings.json. The scripts detect that and \
             stay silent, and termic additionally sets GROK_CLAUDE_HOOKS_ENABLED=false \
             for grok tabs it launches."
                .into(),
        );
    }
    if agent_id == "grok" {
        notes.push(
            "grok tabs launched by termic also get GROK_CLAUDE_HOOKS_ENABLED=false, \
             so grok stops reading claude's hook config and no event fires twice."
                .into(),
        );
    }
    Ok(HookPlan {
        supported: SUPPORTED.contains(&agent_id.as_str()),
        config_path: settings_path(&target).map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        config_is_shared: shared,
        config_fragment: fragment,
        entries,
        notes,
        agent_id,
    })
}

#[tauri::command]
pub fn agent_hooks_status(agent_id: String) -> AgentHookStatus {
    AgentHookStatus {
        supported: SUPPORTED.contains(&base_of(&agent_id).as_str()),
        host: status(&Target::Host(agent_id.clone())),
        docker: status(&Target::Docker(agent_id.clone())),
        agent_id,
    }
}

/// Bring every ALREADY-INSTALLED agent's hooks up to this build's set.
///
/// The user's consent is "hooks on for this agent", not "these exact three
/// scripts". Asking them to notice a version number and press a button to get a
/// fix is asking them to do our job, and it fails silently: an install from an
/// older termic keeps looking installed while reporting less than it could. The
/// heartbeat that stops a cleared spinner staying gone shipped this way, and
/// every existing install would have missed it.
///
/// Only touches agents that are already installed, so it never introduces hooks
/// for an agent the user declined, and it re-runs `install`, which preserves the
/// pre-install backup (`if !backup.exists()`), so clean removal survives.
/// Refuses the same cases install refuses: an unreadable config or
/// `disableAllHooks` is left exactly as found.
///
/// Returns the agent ids it updated, so a caller can say what happened.
#[tauri::command]
pub fn agent_hooks_sync() -> Vec<String> {
    let mut updated = Vec::new();
    // Every agent in the registry, not just the built-in names: a clone is
    // exactly as entitled to a working set of hooks as what it was copied from,
    // and it is the clone whose config dir may have moved.
    let ids: Vec<String> = crate::load_settings_inner()
        .agents
        .iter()
        .map(|a| a.id.clone())
        .collect();
    for agent in &ids {
        for target in [
            Target::Host(agent.clone()),
            Target::Docker(agent.clone()),
        ] {
            let st = status(&target);
            // `ours_present`, not `installed`: see its doc comment. Gating
            // an upgrade on the CURRENT set already being complete means an
            // upgrade can never add an event, which is most of what an
            // upgrade is for.
            if !st.ours_present || !st.stale || st.error.is_some() || st.disabled_all {
                continue;
            }
            if install(&target).is_ok() && matches!(target, Target::Host(_)) {
                updated.push(agent.clone());
            }
        }
    }
    updated
}

/// Install for one agent, covering host AND its Docker config dir. Docker needs
/// no separate consent (termic owns that dir) but must never be installed for an
/// agent the user declined, which is why it rides this one call.
#[tauri::command]
pub fn agent_hooks_install(agent_id: String) -> Result<AgentHookStatus, String> {
    check_supported(&agent_id)?;
    install(&Target::Host(agent_id.clone()))?;
    // A Docker failure must not leave the host half installed and the UI lying,
    // so roll the host back and report the real error.
    if let Err(e) = install(&Target::Docker(agent_id.clone())) {
        let _ = remove(&Target::Host(agent_id.clone()));
        return Err(format!("installed for the host but not for Docker, so nothing was kept: {e}"));
    }
    Ok(agent_hooks_status(agent_id))
}

/// Remove for one agent, both targets. Deliberately NOT gated on `SUPPORTED`:
/// a user who downgrades termic, or who had a since-dropped agent wired, must
/// still be able to clean up.
#[tauri::command]
pub fn agent_hooks_remove(agent_id: String) -> Result<AgentHookStatus, String> {
    let host = remove(&Target::Host(agent_id.clone()));
    let docker = remove(&Target::Docker(agent_id.clone()));
    host.and(docker)?;
    Ok(agent_hooks_status(agent_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "/home/u/.claude/termic-hooks/";
    const C: &str = "/home/u/.claude/termic-hooks/attention.sh";
    /// The tests below all exercise the claude shape. grok's differs only in
    /// which event key it writes under, which `grok_writes_its_own_event` pins.
    const EVENT: &str = "PermissionRequest";
    /// The real message for `EVENT`'s signal, so the merge tests carry what
    /// actually ships rather than a placeholder.
    const SM: &str = "termic: reporting that you are needed";

    fn ours_count(v: &Value) -> usize {
        v.get("hooks")
            .and_then(|h| h.get(EVENT))
            .and_then(Value::as_array)
            .map(|groups| {
                groups
                    .iter()
                    .filter_map(|g| g.get("hooks").and_then(Value::as_array))
                    .flatten()
                    .filter(|e| is_ours(e, P))
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn installs_into_an_empty_config() {
        let out = merge(&serde_json::json!({}), C, P, EVENT, SM);
        assert_eq!(ours_count(&out), 1);
        let entry = &out["hooks"][EVENT][0]["hooks"][0];
        assert_eq!(entry["type"], "command");
        assert_eq!(entry["command"], C);
        // Never a control field: we observe, we do not gate.
        assert!(entry.get("decision").is_none());
        assert!(entry.get("async").is_none());
    }

    #[test]
    fn users_own_hooks_survive_verbatim() {
        let before = serde_json::json!({
            "model": "opus",
            "hooks": {
                "PermissionRequest": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/audit" }] }
                ],
                "Stop": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/done" }] }
                ]
            }
        });
        let after = merge(&before, C, P, EVENT, SM);
        assert_eq!(after["model"], "opus", "unknown top-level keys preserved");
        assert_eq!(after["hooks"]["Stop"], before["hooks"]["Stop"], "other events untouched");
        assert_eq!(
            after["hooks"][EVENT][0], before["hooks"][EVENT][0],
            "the user's own entry for OUR event is untouched"
        );
        assert_eq!(ours_count(&after), 1);
    }

    #[test]
fn an_install_missing_a_newly_added_event_is_still_ours() {
        // The regression that broke the upgrade the first time an event was
        // added. `installed` is an ALL over today's set, so a config written
        // before `SessionStart` existed fails it - and sync skipping anything
        // not installed meant the agent the new event was FOR was the one
        // agent that never got it. Consent is `ours_present`, and that is what
        // sync gates on.
        let mut cfg = serde_json::json!({});
        for ev in ["UserPromptSubmit", "PreToolUse", "PermissionRequest", "Stop"] {
            cfg = merge(&cfg, &format!("{P}{ev}.sh"), P, ev, SM);
        }
        let hooks = hooks_for("claude");
        let has = |root: &Value, event: &str| {
            root.get("hooks").and_then(|h| h.get(event)).and_then(Value::as_array)
                .is_some_and(|groups| groups.iter().any(|g| {
                    g.get("hooks").and_then(Value::as_array)
                        .is_some_and(|l| l.iter().any(|e| is_ours(e, P)))
                }))
        };
        // Exactly the shape `status` computes, without needing a real HOME.
        let installed = hooks.iter().all(|(e, _)| has(&cfg, e));
        let ours_present = hooks.iter().any(|(e, _)| has(&cfg, e));
        assert!(!installed, "a v3 config should NOT satisfy the v4 set");
        assert!(ours_present, "a v3 config is still ours, and still needs upgrading");
    }

    #[test]
fn a_v3_config_gains_the_readiness_event_without_losing_the_others() {
        // What the self-upgrade actually has to do. A config written by the
        // build before Ready already carries our four entries; syncing must
        // ADD SessionStart and leave the rest byte-identical, not rewrite the
        // file wholesale and not append a second copy of anything.
        let mut cfg = serde_json::json!({});
        let v3 = ["UserPromptSubmit", "PreToolUse", "PermissionRequest", "Stop"];
        for ev in v3 {
            cfg = merge(&cfg, &format!("{P}{ev}.sh"), P, ev, SM);
        }
        let before = cfg.clone();
        // The sync: re-run install for the CURRENT set, which now includes
        // the readiness event.
        let mut after = cfg;
        for (ev, _sig) in hooks_for("claude") {
            after = merge(&after, &format!("{P}{ev}.sh"), P, ev, SM);
        }
        assert!(after["hooks"]["SessionStart"].is_array(), "readiness event not added");
        for ev in v3 {
            assert_eq!(after["hooks"][ev], before["hooks"][ev], "{ev} was disturbed by the upgrade");
        }
        // Re-running it changes nothing: sync runs on every launch.
        let mut again = after.clone();
        for (ev, _sig) in hooks_for("claude") {
            again = merge(&again, &format!("{P}{ev}.sh"), P, ev, SM);
        }
        assert_eq!(again, after, "sync is not idempotent, so every launch rewrites the config");
    }

    #[test]
        fn install_is_idempotent() {
        let once = merge(&serde_json::json!({}), C, P, EVENT, SM);
        let twice = merge(&once, C, P, EVENT, SM);
        assert_eq!(ours_count(&twice), 1, "no duplicate entry");
        assert_eq!(once, twice);
    }

    #[test]
    fn an_older_entry_of_ours_is_replaced_not_appended() {
        let stale = serde_json::json!({
            "hooks": { EVENT: [
                { "hooks": [{ "type": "command", "command": format!("{P}old-name.sh"), "timeout": 1 }] }
            ]}
        });
        let out = merge(&stale, C, P, EVENT, SM);
        assert_eq!(ours_count(&out), 1);
        assert_eq!(out["hooks"][EVENT][0]["hooks"][0]["command"], C);
    }

    #[test]
    fn removal_restores_the_original_value() {
        let before = serde_json::json!({
            "model": "opus",
            "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "/x" }] }] }
        });
        let after = merge(&before, C, P, EVENT, SM);
        let back = unmerge(&after, P, EVENT).expect("something of ours to remove");
        assert_eq!(back, before, "byte-identical after a round trip");
    }

    #[test]
    fn removal_from_a_config_that_was_empty_leaves_it_empty() {
        let after = merge(&serde_json::json!({}), C, P, EVENT, SM);
        let back = unmerge(&after, P, EVENT).unwrap();
        assert_eq!(back, serde_json::json!({}), "our own hooks key is cleaned up");
    }

    #[test]
    fn removal_is_a_noop_when_nothing_is_ours() {
        let theirs = serde_json::json!({
            "hooks": { EVENT: [{ "hooks": [{ "type": "command", "command": "/usr/local/bin/audit" }] }] }
        });
        assert!(unmerge(&theirs, P, EVENT).is_none(), "left completely alone");
    }

    #[test]
    fn a_user_edited_entry_of_ours_is_still_matched_by_path() {
        // They changed the timeout and the status message but not the path.
        let edited = serde_json::json!({
            "hooks": { EVENT: [
                { "hooks": [{ "type": "command", "command": C, "timeout": 99, "note": "mine now" }] }
            ]}
        });
        assert_eq!(unmerge(&edited, P, EVENT).unwrap(), serde_json::json!({}));
    }

    #[test]
    fn a_user_entry_sharing_our_group_survives_removal() {
        let mixed = serde_json::json!({
            "hooks": { EVENT: [
                { "hooks": [
                    { "type": "command", "command": C },
                    { "type": "command", "command": "/usr/local/bin/audit" }
                ]}
            ]}
        });
        let back = unmerge(&mixed, P, EVENT).unwrap();
        assert_eq!(ours_count(&back), 0);
        assert_eq!(back["hooks"][EVENT][0]["hooks"][0]["command"], "/usr/local/bin/audit");
    }

    #[test]
    fn a_non_object_root_does_not_panic() {
        let out = merge(&serde_json::json!([1, 2, 3]), C, P, EVENT, SM);
        assert_eq!(ours_count(&out), 1);
    }

    #[test]
    fn disable_all_hooks_is_detected() {
        assert!(disable_all_hooks(&serde_json::json!({ "disableAllHooks": true })));
        assert!(!disable_all_hooks(&serde_json::json!({ "disableAllHooks": false })));
        assert!(!disable_all_hooks(&serde_json::json!({})));
    }

    #[test]
    fn docker_writes_the_container_path_not_the_host_path() {
        let host = command_prefix(&Target::Host("claude".into())).unwrap();
        let docker = command_prefix(&Target::Docker("claude".into())).unwrap();
        assert!(docker.starts_with(crate::docker::CONTAINER_HOME), "{docker}");
        assert!(docker.ends_with("termic-hooks/"));
        assert_ne!(host, docker, "a host path would not resolve inside the cage");
        // The host FILE still lands in the termic-owned docker-agents dir.
        let dir = config_dir(&Target::Docker("claude".into())).unwrap();
        assert!(dir.to_string_lossy().contains("docker-agents"), "{dir:?}");
    }

    #[test]
    fn a_cloned_agent_gets_its_own_directory() {
        let a = config_dir(&Target::Docker("claude".into())).unwrap();
        let b = config_dir(&Target::Docker("claude-review".into())).unwrap();
        assert_ne!(a, b, "clones keep their own login state");
        // ...but both write claude's container path, because the SHAPE is claude's.
        assert_eq!(
            command_prefix(&Target::Docker("claude".into())).unwrap(),
            command_prefix(&Target::Docker("claude-review".into())).unwrap()
        );
    }

    #[test]
    fn the_script_gates_on_both_env_vars_and_always_exits_zero() {
        let s = script_body("claude", Signal::Attention);
        assert!(s.starts_with("#!/bin/sh\n"));
        assert!(s.contains(r#"[ -n "$TERMIC_TASK_ID" ] || exit 0"#));
        assert!(s.contains(r#"[ -z "$GROK_HOOK_EVENT" ] || exit 0"#));
        assert!(s.contains("exit 0\n"));
        // Never raw control bytes in a generated file, whichever form it takes.
        assert!(s.contains("]777;notify;termic;"));
        assert!(!s.contains('\u{1b}'), "no raw ESC in the generated file");
        assert!(!s.contains('\u{7}'), "no raw BEL in the generated file");
        // The body must not match BUILTIN_NOTIFY_IGNORE.claude or the
        // notification is filtered out and the feature dies silently.
        assert!(!s.contains("is waiting for your input"));
    }

    // ── grok ────────────────────────────────────────────────────────
    // grok is the second agent, and almost every difference from claude is
    // load-bearing rather than cosmetic.

    #[test]
    fn grok_writes_its_own_event_and_its_own_file() {
        // grok has NO PermissionRequest. Its attention edge is Notification
        // with notificationType=permission_prompt, measured.
        // Attention is the edge each of these two exists for.
        assert!(hooks_for("grok").contains(&("Notification", Signal::Attention)));
        assert!(hooks_for("claude").contains(&("PermissionRequest", Signal::Attention)));
        // grok scans every *.json under hooks/, so it gets a file of its own:
        // removal is then a delete rather than a merge-back into a file the
        // user also owns.
        assert_eq!(settings_rel("grok"), "hooks/termic.json");
        assert_eq!(settings_rel("claude"), "settings.json");
    }

    /// The chain has to survive a target that does not exist, which is exactly
    /// the Docker case: `$TERMIC_PTY` names a host device the container has no
    /// entry for, so the first redirection fails and the next must be tried.
    #[test]
    fn a_dead_first_target_falls_through_to_the_next() {
        let body = script_body("claude", Signal::Working);
        // `||` chaining on redirection failure, NOT a readiness test:
        // `[ -w /dev/tty ]` is true in places where opening it fails, so
        // attempting the write is the only honest test.
        assert!(body.contains("emit \"$TERMIC_PTY\" || emit /proc/1/fd/1 || emit /dev/tty"),
                "targets must chain on failure:\n{body}");
        assert!(!body.contains("[ -w"), "a readiness test would lie about /dev/tty");
        // Still gated: an agent termic did not spawn writes nothing at all,
        // wherever it is running.
        assert!(body.contains("[ -n \"$TERMIC_PTY\" ] || exit 0"));
        assert!(body.contains("[ -n \"$TERMIC_TASK_ID\" ] || exit 0"));
    }

    #[test]
    fn grok_writes_to_the_pty_because_it_has_no_terminal_sequence() {
        // Every agent is on the pty, claude included: its runtime allowlist
        // drops OSC 133, so its own channel cannot carry a hard done.
        let g = script_body("grok", Signal::Attention);
        assert!(g.contains("$TERMIC_PTY"), "grok must write to the injected pty");
        assert!(!g.contains("terminalSequence"), "grok's runtime ignores that field");
        // $TERMIC_PTY is FIRST and the others are fallbacks, which is the whole
        // ordering: a host agent must never route around the pty it was given.
        let pty_at = g.find("$TERMIC_PTY").expect("pty target");
        for later in ["/proc/1/fd/1", "/dev/tty"] {
            assert!(g.find(later).expect(later) > pty_at, "{later} must come after $TERMIC_PTY");
        }
        // This used to assert /dev/tty was absent, on the grounds that hooks run
        // with no controlling terminal (measured on grok, rc=1). Still true, and
        // that is why it is LAST rather than why it is excluded: a failed open
        // costs one syscall. The reason the chain exists at all is Docker, where
        // $TERMIC_PTY is a host path the container cannot see, measured in the
        // real sandbox image as TERMIC_PTY_EXISTS=NO.
        let c = script_body("claude", Signal::Attention);
        assert!(c.contains("$TERMIC_PTY"));
        assert!(!c.contains("terminalSequence"));
    }

    #[test]
    fn the_two_scripts_gate_on_grok_in_opposite_directions() {
        // grok reads ~/.claude/settings.json too, so claude's script must stay
        // silent when grok is the caller or a claude install silently rewires
        // grok. grok's own script wants exactly the opposite test.
        assert!(script_body("claude", Signal::Attention).contains(r#"[ -z "$GROK_HOOK_EVENT" ] || exit 0"#));
        assert!(script_body("grok", Signal::Attention).contains(r#"[ -n "$GROK_HOOK_EVENT" ] || exit 0"#));
        // Both still refuse to speak outside a termic PTY.
        for a in ["claude", "grok"] {
            assert!(script_body(a, Signal::Attention).contains(r#"[ -n "$TERMIC_TASK_ID" ] || exit 0"#));
            assert!(script_body(a, Signal::Attention).trim_end().ends_with("exit 0"));
            assert!(!script_body(a, Signal::Attention).contains('\u{1b}'), "no raw ESC in {a}'s script");
        }
    }

    #[test]
    fn claude_registers_a_readiness_event() {
        // The one signal that is not a correction of something the terminal
        // got wrong, but a state it cannot express at all. Without it the
        // first message is typed on a quiet-terminal guess, and claude's trust
        // picker is quiet with `No, exit` highlighted.
        let set = hooks_for("claude");
        assert!(set.contains(&("SessionStart", Signal::Ready)),
            "claude lost its readiness hook: {set:?}");
        // Only claude: nobody else was measured, and a guessed event name
        // installs a hook that never fires and looks like one that does.
        for agent in ["grok", "agy", "opencode"] {
            assert!(!hooks_for(agent).iter().any(|(_, s)| *s == Signal::Ready),
                "{agent} claims Ready without a measurement behind it");
        }
    }

    #[test]
    fn ready_and_attention_share_an_osc_but_never_a_body() {
        // They are told apart on the TypeScript side by BODY alone
        // (`lib/agentHooks.ts`), so neither may be a prefix of the other or a
        // ready session badges as needing you.
        let ready = Signal::Ready.payload();
        let attn = Signal::Attention.payload();
        let pre = "777;notify;termic;";
        assert!(ready.starts_with(pre) && attn.starts_with(pre));
        let (rb, ab) = (&ready[pre.len()..], &attn[pre.len()..]);
        assert!(!rb.starts_with(ab) && !ab.starts_with(rb), "{rb:?} vs {ab:?} are confusable");
        // Pinned against HOOK_OSC_READY_BODY, which cannot import this. The
        // two constants are the halves of one contract across the boundary.
        assert_eq!(rb, "agent ready for input");
        // Each signal needs its own script filename or one overwrites another.
        let stems = [Signal::Attention, Signal::Working, Signal::Done, Signal::Ready]
            .map(|s| s.stem());
        let mut uniq = stems.to_vec();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), stems.len(), "duplicate script stem: {stems:?}");
        // User-visible in claude's UI while the hook runs, so it has to
        // describe the signal actually being sent.
        assert_ne!(Signal::Ready.status_message(), Signal::Attention.status_message());
    }

    #[test]
    fn the_schema_bump_is_what_makes_sync_replace_old_installs() {
        // The upgrade path rests entirely on this. An install from the build
        // before the current set must read as stale, or `agent_hooks_sync`
        // skips it and the user keeps that set forever: v3 types into startup
        // dialogs, v4 holds a tab on `working` for the rest of the session.
        assert_eq!(SCHEMA_VERSION, 6, "bump me with the hook set, or installs go stale silently");
    }

    #[test]
    fn every_supported_agent_has_an_event_and_a_state_dir() {
        for a in SUPPORTED {
            assert!(!hooks_for(a).is_empty(), "{a} is listed as supported but has no events");
            assert!(state_dir(a).is_ok(), "{a} has no state dir, so nowhere to install");
            // Install targets must be global or termic-owned, never a repo.
            let host = config_dir(&Target::Host((*a).into())).unwrap();
            assert!(!host.to_string_lossy().contains("worktree"), "{a}: {host:?}");
        }
    }

    // The two states that matter most must come from a PROTOCOL on every
    // agent that has one, never from the terminal. The terminal is a heuristic
    // that changes when a vendor changes their UI: the Codex latch in
    // docs/gotchas.md is exactly that failure, and it silently disabled done
    // detection until someone noticed tabs stuck on "working".
    #[test]
    fn done_comes_from_a_hook_on_every_agent_that_can_report_it() {
        for agent in SUPPORTED {
            let h = hooks_for(agent);
            if *agent == "agy" || *agent == "opencode" || *agent == "claude" || *agent == "grok" {
                assert!(
                    h.iter().any(|(_, s)| *s == Signal::Done),
                    "{agent} must report done via a hook, not the title"
                );
            }
            // Done is ignored unless we were working, so an agent reporting
            // Done must report Working too or its done never fires.
            if h.iter().any(|(_, s)| *s == Signal::Done) {
                assert!(
                    h.iter().any(|(_, s)| *s == Signal::Working),
                    "{agent} reports Done with no Working, so Done can never fire"
                );
            }
        }
    }

    #[test]
    fn attention_comes_from_a_hook_wherever_the_agent_has_such_an_event() {
        for agent in ["claude", "grok", "opencode"] {
            assert!(
                hooks_for(agent).iter().any(|(_, s)| *s == Signal::Attention),
                "{agent} has an attention-shaped event and must use it"
            );
        }
        // agy is the documented exception: no attention-shaped event exists,
        // so needs-you there stays on termic's byte-quiet fallback.
        assert!(!hooks_for("agy").iter().any(|(_, s)| *s == Signal::Attention));
    }

    #[test]
    fn only_grok_can_report_an_interrupt() {
        // Measured: ESC mid-turn fires NOTHING on claude or codex, so their
        // done cannot survive an interrupt and OSC stays the backstop there.
        // grok's StopCancelled is the one exception.
        assert!(hooks_for("grok").iter().any(|(e, _)| *e == "StopCancelled"));
        assert!(!hooks_for("claude").iter().any(|(e, _)| *e == "StopCancelled"));
    }

    // The reason done must come from the protocol, in one test.
    //
    // Measured: claude backgrounds two shells, ends the parent turn, and paints
    // its IDLE glyph 49.6 SECONDS before the work actually finishes. On a
    // 1-2 hour subagent run that is 1-2 hours of a confident, wrong "done".
    // Its `Stop` fires three times there, and only the third has an empty
    // `background_tasks`. agy says the same thing as `fullyIdle`.
    #[test]
    fn done_hooks_refuse_to_fire_while_work_is_outstanding() {
        let claude = script_body("claude", Signal::Done);
        assert!(claude.contains("background_tasks"), "claude's done must consult its payload");
        // A WHITELIST, not a non-empty test. The delegated types hold the turn
        // open; anything else, named or not, lets done through. See the guard.
        for held in ["subagent", "workflow", "shell", "teammate"] {
            assert!(claude.contains(&format!(r#""type":"{held}""#)),
                "{held} must hold the turn open");
        }
        // The types that never end must NOT appear. `monitor` is the artifact
        // watch (an ambient websocket monitor); it outlives every turn.
        for never in ["monitor", "dream", "auto-modescan"] {
            assert!(!claude.contains(&format!(r#""type":"{never}""#)),
                "{never} must never hold the turn open");
        }
        // Sliced out of the array, not matched across the whole payload:
        // `last_assistant_message` is the agent's own prose.
        assert!(claude.contains(r#"tasks=${flat##*'"background_tasks":['}"#),
            "the array must be sliced before matching");

        let agy = script_body("agy", Signal::Done);
        assert!(agy.contains("fullyIdle"), "agy's done must consult its payload");

        // No jq: the scripts must keep working on a machine that has none.
        for a in ["claude", "agy"] {
            assert!(!script_body(a, Signal::Done).contains("jq"));
        }
        // Only Done inspects a payload. Working and attention are unconditional.
        assert!(!script_body("claude", Signal::Working).contains("background_tasks"));
        assert!(!script_body("claude", Signal::Attention).contains("background_tasks"));
    }

    /// Run claude's generated Done script against a real `Stop` payload and
    /// report whether it emitted. `TERMIC_PTY` points at a temp file, which is
    /// exactly how the script addresses a pty: a plain path it redirects into.
    ///
    /// This executes the shell rather than asserting on the source, because
    /// every bug this guard has had was a semantic one that a substring
    /// assertion happily agreed with.
    #[cfg(unix)]
    fn done_emits_for(payload: &str) -> bool {
        use std::io::Read;
        use std::process::{Command, Stdio};

        let dir = std::env::temp_dir().join(format!(
            "termic-hook-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("done.sh");
        let pty = dir.join("pty");
        std::fs::write(&script, script_body("claude", Signal::Done)).unwrap();
        std::fs::write(&pty, "").unwrap();

        let mut child = Command::new("/bin/sh")
            .arg(&script)
            .env("TERMIC_TASK_ID", "t1")
            .env("TERMIC_PTY", &pty)
            .env_remove("GROK_HOOK_EVENT")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sh");
        {
            use std::io::Write as _;
            child.stdin.as_mut().unwrap().write_all(payload.as_bytes()).unwrap();
        }
        assert!(child.wait().unwrap().success(), "a hook must never exit non-zero");

        let mut out = String::new();
        std::fs::File::open(&pty).unwrap().read_to_string(&mut out).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        out.contains("133;D")
    }

    /// Run claude's generated ATTENTION script against a payload and return the
    /// body it put on the pty. Same harness as `done_emits_for` and for the same
    /// reason: the extraction is shell, and shell is where the bugs are.
    #[cfg(unix)]
    fn attention_body_for(payload: &str) -> String {
        attention_body_for_agent("claude", payload)
    }

    #[cfg(unix)]
    fn attention_body_for_agent(agent: &str, payload: &str) -> String {
        use std::io::Read;
        use std::process::{Command, Stdio};

        let dir = std::env::temp_dir().join(format!(
            "termic-attn-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("attention.sh");
        let pty = dir.join("pty");
        std::fs::write(&script, script_body(agent, Signal::Attention)).unwrap();
        std::fs::write(&pty, "").unwrap();

        let mut child = Command::new("/bin/sh")
            .arg(&script)
            .env("TERMIC_TASK_ID", "t1")
            .env("TERMIC_PTY", &pty)
            .env_remove("GROK_HOOK_EVENT")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sh");
        {
            use std::io::Write as _;
            child.stdin.as_mut().unwrap().write_all(payload.as_bytes()).unwrap();
        }
        assert!(child.wait().unwrap().success(), "a hook must never exit non-zero");

        let mut out = String::new();
        std::fs::File::open(&pty).unwrap().read_to_string(&mut out).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        // Strip introducer/terminator and the sender fields, leaving the body.
        let start = out.find(NOTIFY_PREFIX).map(|i| i + NOTIFY_PREFIX.len());
        match start {
            Some(i) => out[i..].trim_end_matches('\u{7}').to_string(),
            None => String::new(),
        }
    }

    /// A `PermissionRequest` payload in the field order claude's own embedded
    /// hooks reference documents for the tool events (2.1.259: `session_id`,
    /// `tool_name`, `tool_input`). Values are synthetic.
    #[cfg(unix)]
    fn permission_payload(tool: &str, input: &str) -> String {
        format!(
            r#"{{"session_id":"s1","transcript_path":"/Users/u/.claude/x.jsonl",
               "cwd":"/Users/u/proj","hook_event_name":"PermissionRequest",
               "tool_name":"{tool}","tool_input":{input}}}"#
        )
    }

    // GH #276, the second half. The hook is what the user actually SEES: it
    // fires the moment claude blocks, and claude's own OSC 9 arrives 6.0s
    // behind it, too late to compose the banner and rightly unable to raise a
    // second one. So the hook's body has to carry the useful part.
    #[test]
    #[cfg(unix)]
    fn claude_attention_names_the_tool_it_is_blocked_on() {
        assert_eq!(
            attention_body_for(&permission_payload("Bash", r#"{"command":"rm -rf build"}"#)),
            "needs your permission: Bash",
        );
        // An MCP tool id is still a bare identifier, and is the case most worth
        // naming: "needs your permission" alone tells you nothing about which
        // of six servers is asking.
        assert_eq!(
            attention_body_for(&permission_payload("mcp__github__create_pr", "{}")),
            "needs your permission: mcp__github__create_pr",
        );
    }

    #[test]
    #[cfg(unix)]
    fn claude_attention_falls_back_rather_than_going_silent() {
        // Events that are not tool events carry no `tool_name` (SessionStart,
        // Stop, UserPromptSubmit). They must still notify, with the old body.
        let no_tool = r#"{"session_id":"s1","hook_event_name":"Notification",
                          "message":"Claude needs your permission"}"#;
        assert_eq!(attention_body_for(no_tool), ATTENTION_BODY);
        // Empty stdin is the degenerate case a runtime change could produce.
        assert_eq!(attention_body_for(""), ATTENTION_BODY);
    }

    #[test]
    #[cfg(unix)]
    fn claude_attention_rejects_a_tool_name_that_would_corrupt_the_payload() {
        // A `;` would split OSC 777 into the wrong fields, and the body sits in
        // the LAST field, so an injected one silently re-points the sender: a
        // body of `termic;...` in the title position is how a hostile payload
        // would forge a trusted signal. Rejected wholesale rather than
        // sanitised, so there is no escaping rule to get subtly wrong.
        assert_eq!(
            attention_body_for(&permission_payload("Bash;notify;termic;pwned", "{}")),
            ATTENTION_BODY,
        );
        // Same for a printf format specifier: the body goes through `%s` so it
        // could not reach the format string anyway, and this pins BOTH guards.
        assert_eq!(attention_body_for(&permission_payload("%s%s%n", "{}")), ATTENTION_BODY);
        // Whitespace is NOT rejected, it is gone before the check: the payload
        // is flattened first (same `tr -d '[:space:]'` the Done guard uses), so
        // an interior space is deleted rather than caught. Asserted because it
        // is surprising, and left alone because it is harmless - the extraction
        // stops at the closing quote, so the worst case is two words glued into
        // one identifier in a banner, never a field separator.
        assert_eq!(
            attention_body_for(&permission_payload("Bash Write", "{}")),
            "needs your permission: BashWrite",
        );
    }

    /// codex's `PermissionRequest` payload, transcribed from the shape a live
    /// 0.153.0 emitted at a real approval prompt. Placeholders throughout: the
    /// captured one carried a real home path, a real session id and a real
    /// transcript path, none of which belong in this repo.
    ///
    /// The shape is the point. `tool_name` sits AFTER `hook_event_name` and
    /// before `tool_input`, and `tool_input.description` is free-form prose
    /// from the model, which is exactly the field that would break a naive
    /// last-occurrence match.
    #[cfg(unix)]
    fn codex_permission_payload(tool: &str) -> String {
        format!(
            r#"{{"session_id":"00000000-0000-0000-0000-000000000000",
               "turn_id":"00000000-0000-0000-0000-000000000001",
               "transcript_path":"/Users/u/.codex/sessions/2026/01/01/rollout.jsonl",
               "cwd":"/Users/u/proj","hook_event_name":"PermissionRequest",
               "model":"gpt-5","permission_mode":"default","tool_name":"{tool}",
               "tool_input":{{"command":"echo hello > out.txt",
               "description":"Do you want to allow creating out.txt?"}}}}"#
        )
    }

    #[test]
    #[cfg(unix)]
    fn codex_attention_names_the_tool_too() {
        // Measured end to end before this was written: driving a real codex TUI
        // to an approval prompt fired PermissionRequest with `"tool_name":"Bash"`
        // at the instant the prompt painted.
        assert_eq!(
            attention_body_for_agent("codex", &codex_permission_payload("Bash")),
            "needs your permission: Bash",
        );
        // And the same guards apply, since it is literally the same script body.
        assert_eq!(
            attention_body_for_agent("codex", &codex_permission_payload("a;notify;termic;x")),
            ATTENTION_BODY,
        );
    }

    /// Run codex's generated READY script and return everything it put on the
    /// pty, both sequences.
    #[cfg(unix)]
    fn ready_output_for(payload: &str) -> String {
        use std::io::Read;
        use std::process::{Command, Stdio};

        let dir = std::env::temp_dir().join(format!(
            "termic-ready-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("ready.sh");
        let pty = dir.join("pty");
        std::fs::write(&script, script_body("codex", Signal::Ready)).unwrap();
        std::fs::write(&pty, "").unwrap();
        let mut child = Command::new("/bin/sh")
            .arg(&script)
            .env("TERMIC_TASK_ID", "t1")
            .env("TERMIC_PTY", &pty)
            .env_remove("GROK_HOOK_EVENT")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sh");
        {
            use std::io::Write as _;
            child.stdin.as_mut().unwrap().write_all(payload.as_bytes()).unwrap();
        }
        assert!(child.wait().unwrap().success(), "a hook must never exit non-zero");
        let mut out = String::new();
        std::fs::File::open(&pty).unwrap().read_to_string(&mut out).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    /// codex's `SessionStart` payload, transcribed from a live 0.153.0 with
    /// placeholders for the paths. Field ORDER matters: `session_id` comes
    /// first, ahead of `transcript_path`, and `transcript_path` embeds the same
    /// uuid a second time, which is what a last-occurrence match would find.
    #[cfg(unix)]
    fn codex_session_start_payload(sid: &str) -> String {
        format!(
            r#"{{"session_id":"{sid}",
               "transcript_path":"/Users/u/.codex/sessions/2026/01/01/rollout-2026-01-01T00-00-00-{sid}.jsonl",
               "cwd":"/Users/u/proj","hook_event_name":"SessionStart",
               "model":"gpt-5","permission_mode":"default","source":"startup"}}"#
        )
    }

    // Repo-root resume for codex hangs entirely off this one line of shell: no
    // id reported means no id stored, which means `resume --last` and the wrong
    // task's conversation.
    #[test]
    #[cfg(unix)]
    fn codex_ready_reports_the_session_id_alongside_ready() {
        let sid = "01a06adc-eeb5-77a0-b603-d7b670dd11e7";
        let out = ready_output_for(&codex_session_start_payload(sid));
        assert!(
            out.contains(&format!("{NOTIFY_PREFIX}{READY_BODY}")),
            "ready must still be sent, unchanged: {out:?}"
        );
        assert!(
            out.contains(&format!("{NOTIFY_PREFIX}{SESSION_BODY_PREFIX}{sid}")),
            "the session id never made it to the pty: {out:?}"
        );
        // Ready FIRST. `seedPrompt` waits on it, and an id arriving ahead of it
        // would be the tab reporting a session before it reports being usable.
        assert!(
            out.find(READY_BODY).unwrap() < out.find(SESSION_BODY_PREFIX).unwrap(),
            "ready must precede the session id: {out:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn codex_ready_still_reports_ready_when_there_is_no_usable_id() {
        // Ready is the load-bearing half: `seedPrompt` refuses to type into an
        // agent that reports readiness and has not reported it, so a payload
        // this cannot parse must NOT cost the tab its ready signal.
        for payload in [
            String::new(),
            r#"{"hook_event_name":"SessionStart","cwd":"/Users/u/p"}"#.to_string(),
            // Not a uuid: rejected rather than escaped, because it would be
            // expanded into a `resume <id>` command line.
            codex_session_start_payload("not-a-uuid"),
            codex_session_start_payload("../../etc/passwd"),
            codex_session_start_payload("$(rm -rf /)"),
        ] {
            let out = ready_output_for(&payload);
            assert!(out.contains(READY_BODY), "ready lost for {payload:?}: {out:?}");
            assert!(
                !out.contains(SESSION_BODY_PREFIX),
                "a bad id must not be reported: {out:?}"
            );
        }
    }

    // The body must survive the TS side, which is the failure mode with no
    // symptom: the hook fires correctly and termic drops it on the floor.
    #[test]
    fn the_enriched_attention_body_is_still_not_claudes_idle_nag() {
        // `notificationWantsAttention`'s ignore pattern for claude is "is
        // waiting for your input" (its 60s nudge after an unanswered turn).
        for body in [ATTENTION_BODY, "needs your permission: Bash"] {
            assert!(!body.contains("is waiting for your input"), "{body} would be filtered");
        }
        // And it must never collide with the READY body, which shares the OSC
        // id and the trusted sender and is told apart by an exact match alone.
        assert_ne!(ATTENTION_BODY, READY_BODY);
        assert!(!"needs your permission: Bash".starts_with(READY_BODY));
    }

    /// The whole codex loop against a REAL `codex` binary: install, trust,
    /// verify codex agrees, remove, verify nothing of ours is left.
    ///
    /// `#[ignore]`d because it needs codex on PATH, which CI does not have. Run
    /// it with a real one:
    ///
    /// ```sh
    /// cargo test --features e2e codex_hooks_install -- --ignored --nocapture
    /// ```
    ///
    /// It writes ONLY into a temp dir: `TERMIC_E2E_AGENT_HOME` moves the agent
    /// home, and `CODEX_HOME` follows it because `config_dir` derives one from
    /// the other. The user's own `~/.codex` is never opened.
    ///
    /// The assertion that matters is not "we wrote a trust entry" but "codex
    /// says trusted". A trust entry with the wrong hash is silently `modified`
    /// and the hook never runs, so only codex's own verdict proves the install.
    #[test]
    #[ignore = "needs a real codex binary on PATH"]
    #[cfg(all(unix, feature = "e2e"))]
    fn codex_hooks_install_is_trusted_by_codex_and_leaves_nothing_behind() {
        let home = std::env::temp_dir().join(format!("termic-codex-e2e-{}", std::process::id()));
        let codex_home = home.join(".codex");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::env::set_var("TERMIC_E2E_AGENT_HOME", &home);

        // A config.toml the user "already had", so the trust write has to
        // preserve something rather than starting from an empty file.
        let original_config = "model = \"gpt-5.6-sol\"\n\n[tui]\nnotifications = true\n";
        std::fs::write(codex_home.join("config.toml"), original_config).unwrap();

        let target = Target::Host("codex".into());
        install(&target).expect("install codex hooks");

        // 1. The hooks file exists and holds one group per registered event.
        let hooks_json = codex_home.join("hooks.json");
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&hooks_json).unwrap()).unwrap();
        for (event, _) in hooks_for("codex") {
            assert!(
                v["hooks"][event].as_array().is_some_and(|g| !g.is_empty()),
                "{event} missing from {}",
                hooks_json.display()
            );
        }

        // 2. The user's own config survived, and trust was added beside it.
        let cfg = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert!(cfg.contains("gpt-5.6-sol"), "the user's config was clobbered:\n{cfg}");
        assert!(cfg.contains("[tui]"));
        assert!(cfg.contains("trusted_hash"), "no trust written:\n{cfg}");

        // 3. THE assertion: codex itself agrees. Anything wrong with the key or
        //    the hash shows up here as `untrusted`/`modified`, which is exactly
        //    how this fails in the field - silently.
        let bin = "codex";
        let found = crate::codex_trust::discover_ours(
            bin,
            &codex_home,
            &hooks_json,
            &command_prefix(&target).unwrap(),
            &codex_home,
        )
        .expect("codex app-server hooks/list");
        assert_eq!(
            found.len(),
            hooks_for("codex").len(),
            "codex did not report every hook we wrote: {found:?}"
        );
        for h in &found {
            eprintln!("  codex says: {} -> {}", h.key, h.trust_status);
            assert_eq!(h.trust_status, "trusted", "hook not trusted: {h:?}");
        }

        // 4. Status agrees too, so the UI is not claiming something else.
        assert!(status(&target).installed, "status must see a complete install");

        // 5. The command the UI actually calls, which does BOTH targets and
        //    rolls the host back if the Docker half errors. An earlier version
        //    of the Docker guard returned Err here and made codex hooks
        //    uninstallable everywhere while every direct-install test passed.
        remove(&target).expect("reset before the command-level install");
        let full = agent_hooks_install("codex".into()).expect("agent_hooks_install");
        assert!(full.supported, "codex must report as supported");
        assert!(full.host.installed, "the host half must be installed");
        assert!(
            !full.docker.installed,
            "the Docker half must report OFF rather than claiming hooks that cannot run"
        );

        // 6. Status is HONEST about trust, not just about the hooks file.
        //    Strip the trust the way a user editing config.toml would, and the
        //    row must stop claiming "on": those hooks are still on disk, still
        //    reported `enabled` by codex, and still run nothing.
        let cfg_path = codex_home.join("config.toml");
        let stripped =
            crate::codex_trust::without_trust(&std::fs::read_to_string(&cfg_path).unwrap(), &hooks_json)
                .unwrap();
        std::fs::write(&cfg_path, &stripped).unwrap();
        let untrusted = status(&target);
        assert!(!untrusted.installed, "status must not claim untrusted hooks are on");
        assert!(
            untrusted.error.as_deref().is_some_and(|e| e.contains("trust")),
            "and it must say why: {:?}",
            untrusted.error
        );
        // Re-installing is the documented fix, so it has to actually work.
        install(&target).expect("re-install after trust was stripped");
        assert!(status(&target).installed, "re-install must restore trust");

        // 7. Removal takes the hooks AND the trust with it, and hands the
        //    user's config.toml back byte-identical.
        remove(&target).expect("remove codex hooks");
        let after = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert_eq!(after, original_config, "config.toml must come back unchanged");
        let left = std::fs::read_to_string(&hooks_json).unwrap_or_default();
        assert!(
            !left.contains(SCRIPT_DIR),
            "our commands are still in hooks.json:\n{left}"
        );
        assert!(!status(&target).ours_present, "status still sees our entries");

        std::env::remove_var("TERMIC_E2E_AGENT_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The other half, and the one that cannot be argued from a config file:
    /// does a REAL codex turn actually run these hooks and put termic's OSC on
    /// the terminal it was handed?
    ///
    /// `#[ignore]`d and separate from the install test because it costs a live
    /// model call on the user's own account. Run it deliberately:
    ///
    /// ```sh
    /// cargo test --features e2e codex_hooks_fire -- --ignored --nocapture
    /// ```
    ///
    /// It borrows the user's login with a SYMLINK to `auth.json` rather than a
    /// copy, so no credential is ever duplicated into a temp dir, and the real
    /// `~/.codex` is never written to. `TERMIC_PTY` points at a plain file,
    /// which is how the scripts address a pty anyway (they redirect into a
    /// path), so this needs no terminal.
    #[test]
    #[ignore = "spends a real codex turn on the user's account"]
    #[cfg(all(unix, feature = "e2e"))]
    fn codex_hooks_fire_on_a_real_turn() {
        let home = std::env::temp_dir().join(format!("termic-codex-fire-{}", std::process::id()));
        let codex_home = home.join(".codex");
        let work = home.join("work");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::create_dir_all(&work).unwrap();

        let real_auth = dirs::home_dir().unwrap().join(".codex/auth.json");
        if !real_auth.exists() {
            eprintln!("SKIP: no ~/.codex/auth.json, cannot make a live call");
            return;
        }
        std::os::unix::fs::symlink(&real_auth, codex_home.join("auth.json")).unwrap();

        std::env::set_var("TERMIC_E2E_AGENT_HOME", &home);
        let target = Target::Host("codex".into());
        install(&target).expect("install codex hooks");

        // A REAL pty, not a file. The scripts write with a TRUNCATING redirect
        // (`> "$1"`), which is meaningless on a character device and total on a
        // regular file: pointed at a file, each hook erases the one before it
        // and a three-hook turn ends with only the last sequence on disk. Found
        // exactly that way, and it would have hidden two of the three signals
        // this test exists to prove.
        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize::default())
            .expect("open a pty");
        let pty_path = crate::pty_slave_path(&pair.master).expect("pty slave path");
        let mut reader = pair.master.try_clone_reader().expect("pty reader");
        let seen_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let sink = seen_buf.clone();
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 { break }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        let pty = std::path::PathBuf::from(&pty_path);
        let out = std::process::Command::new("codex")
            .args(["exec", "--skip-git-repo-check", "reply with the single word ok"])
            .current_dir(&work)
            .env("CODEX_HOME", &codex_home)
            .env("TERMIC_PTY", &pty)
            .env("TERMIC_TASK_ID", "live-fire")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run codex exec");
        let transcript = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        // The hooks write asynchronously through a pty; give the reader a beat
        // to drain what the child wrote just before it exited.
        std::thread::sleep(std::time::Duration::from_millis(500));
        let seen = String::from_utf8_lossy(&seen_buf.lock().unwrap()).to_string();
        eprintln!("--- codex said ---\n{}", transcript.chars().take(1200).collect::<String>());
        eprintln!("--- pty got {} bytes ---\n{seen:?}", seen.len());

        // Ready, working and done are the three the state machine cannot run
        // without. Attention needs a permission prompt, which a read-only
        // one-shot never reaches, so it is proven by the install test agreeing
        // codex registered it rather than by firing here.
        assert!(seen.contains("777;notify;termic;agent ready for input"),
            "SessionStart (ready) never reached the pty");
        assert!(seen.contains("133;C"), "working never reached the pty");
        assert!(seen.contains("133;D"), "Stop (done) never reached the pty");
        // Order matters as much as presence: a done before any working is a
        // turn termic would ignore, since a hard idle is dropped unless we
        // were working.
        assert!(seen.find("133;C").unwrap() < seen.rfind("133;D").unwrap(),
            "done arrived before working: {seen:?}");

        // The session id, which is what makes repo-root resume possible at all:
        // several tasks share the repo root's cwd, so `resume --last` there is
        // another task's conversation. codex cannot be HANDED an id at launch,
        // so it has to report the one it chose.
        let reported = seen
            .split("\u{1b}]777;notify;termic;session ")
            .nth(1)
            .and_then(|rest| rest.split('\u{7}').next())
            .map(str::to_string)
            .expect(&format!("no session id reported: {seen:?}"));
        // It must be the id codex actually used, not merely a well-formed one.
        assert!(
            transcript.contains(&reported),
            "reported {reported} is not the session codex announced:\n{transcript}"
        );
        // Ready first: seedPrompt blocks on it, and both ride ONE write, which
        // is why a truncating redirect cannot cost us the ready half.
        assert!(
            seen.find("agent ready for input").unwrap() < seen.find("session ").unwrap(),
            "ready must precede the session id: {seen:?}"
        );

        remove(&target).expect("remove codex hooks");
        std::env::remove_var("TERMIC_E2E_AGENT_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// One payload per background-task type claude can report, in the shape
    /// `Rcr` builds them (2.1.259). Types transcribed from claude's own label
    /// map; the values are synthetic.
    #[cfg(unix)]
    fn stop_payload(tasks: &str) -> String {
        format!(
            r#"{{"session_id":"s1","transcript_path":"/Users/u/.claude/x.jsonl",
               "cwd":"/Users/u/proj","hook_event_name":"Stop","stop_hook_active":false,
               "last_assistant_message":"done","background_tasks":[{tasks}],
               "session_crons":[]}}"#
        )
    }

    // The regression this whole guard exists to not have twice.
    //
    // v4 asked "is background_tasks non-empty". An artifact watch answers yes
    // for the entire session: it is an ambient websocket monitor, it is
    // `running` from the moment a page is published, and claude's Stop payload
    // includes it (the builder's only filter is status running|pending). So one
    // publish silenced every later done, and because hooks had already proven
    // themselves on that pty, no demoter and neither ceiling was left armed to
    // notice. The tab loaded forever, across new turns and finished turns.
    #[test]
    #[cfg(unix)]
    fn done_fires_through_an_artifact_watch() {
        let watch = r#"{"id":"m1","type":"monitor","status":"running",
                        "description":"Artifact live updates"}"#;
        assert!(done_emits_for(&stop_payload(watch)),
            "an artifact watch must NOT hold the spinner: it never ends");
        assert!(done_emits_for(&stop_payload("")), "an empty array is plainly done");
    }

    // The half the guard is right about, kept honest.
    #[test]
    #[cfg(unix)]
    fn done_waits_for_delegated_work() {
        let cases = [
            (r#"{"id":"a1","type":"subagent","status":"running","description":"Explore","agent_type":"Explore"}"#, false),
            (r#"{"id":"w1","type":"workflow","status":"running","description":"review","name":"review"}"#, false),
            (r#"{"id":"b1","type":"shell","status":"running","description":"build","command":"make beta"}"#, false),
            (r#"{"id":"d1","type":"dream","status":"running","description":"dreaming"}"#, true),
            (r#"{"id":"u1","type":"some_future_type","status":"running","description":"?"}"#, true),
        ];
        for (task, should_emit) in cases {
            assert_eq!(done_emits_for(&stop_payload(task)), should_emit,
                "wrong verdict for {task}");
        }
        // A watch alongside a real subagent still waits for the subagent.
        let both = r#"{"id":"m1","type":"monitor","status":"running","description":"Artifact live updates"},
                      {"id":"a1","type":"subagent","status":"running","description":"Explore","agent_type":"Explore"}"#;
        assert!(!done_emits_for(&stop_payload(both)), "the subagent must still hold");
    }

    // The agent's own prose must not be able to hold its spinner down. A turn
    // that discusses `"type":"shell"` (this one did) serialises that text into
    // `last_assistant_message`, which is why the array is sliced out first.
    #[test]
    #[cfg(unix)]
    fn done_ignores_task_types_quoted_in_the_transcript() {
        let payload = r#"{"session_id":"s1","hook_event_name":"Stop","stop_hook_active":false,
            "last_assistant_message":"The whitelist holds for \"type\":\"subagent\" and \"type\":\"shell\".",
            "background_tasks":[],"session_crons":[]}"#;
        assert!(done_emits_for(payload),
            "only the background_tasks array may decide this");
    }

    // ── Antigravity ─────────────────────────────────────────────────
    // Its config is a different SHAPE, not a variation on claude's, and the
    // heterogeneity below is the thing that silently produced empty commands.

    #[test]
    fn agy_reports_done_and_working_because_it_has_no_attention_event() {
        let h = hooks_for("agy");
        assert_eq!(h, &[("PreInvocation", Signal::Working), ("Stop", Signal::Done)]);
        // Working is not optional here: `goIdle(reason, 0)` is ignored unless
        // we were working, so Done alone would never fire.
        assert!(h.iter().any(|(_, s)| *s == Signal::Working));
    }

    /// Claude DISPLAYS `statusMessage` while the hook runs, so a shared string
    /// meant a turn starting announced "you are needed". It shipped that way
    /// and was only caught by installing into a real config and reading the
    /// merged file. Each signal now says what it is actually reporting.
    /// Working is the only SUSTAINED state here, and the only one that needs
    /// re-asserting. The terminal title this replaced repainted constantly, so
    /// a spinner cleared by anything came back within a frame; a single
    /// UserPromptSubmit made the same clear permanent for the whole turn.
    #[test]
    fn working_has_a_heartbeat_wherever_one_is_safe() {
        for agent in ["claude", "grok"] {
            let working: Vec<&str> = hooks_for(agent)
                .iter()
                .filter(|(_, s)| *s == Signal::Working)
                .map(|(e, _)| *e)
                .collect();
            assert!(
                working.len() >= 2,
                "{agent} reports working from one edge only, so a cleared spinner never returns: {working:?}",
            );
            assert!(working.contains(&"PreToolUse"), "{agent}: {working:?}");
        }

        // agy is the deliberate exception, twice over: PreInvocation already
        // fires per model invocation (so it HAS a heartbeat), and its
        // PreToolUse documents `decision` as required, so observing it
        // silently risks blocking the tool.
        let agy: Vec<&str> = hooks_for("agy").iter().map(|(e, _)| *e).collect();
        assert!(!agy.contains(&"PreToolUse"), "agy must not observe PreToolUse: {agy:?}");
        assert!(agy.contains(&"PreInvocation"));
    }

    /// Done and attention are EDGES and deliberately have no heartbeat. A turn
    /// ends once, and a repeated "still done" would re-badge something the user
    /// already dismissed.
    #[test]
    fn done_and_attention_stay_single_edges() {
        for agent in SUPPORTED {
            for sig in [Signal::Done, Signal::Attention] {
                let n = hooks_for(agent).iter().filter(|(_, s)| *s == sig).count();
                // grok is the one agent with two done events, and they are
                // mutually exclusive outcomes of one turn (Stop, StopCancelled),
                // not a repeat of the same one.
                let cap = if *agent == "grok" && sig == Signal::Done { 2 } else { 1 };
                assert!(n <= cap, "{agent} {sig:?} registered {n} times");
            }
        }
    }

    #[test]
    fn each_signal_announces_itself_honestly() {
        let msgs: Vec<&str> = [Signal::Working, Signal::Attention, Signal::Done]
            .iter()
            .map(|s| s.status_message())
            .collect();
        let mut uniq = msgs.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), 3, "every signal needs its own wording: {msgs:?}");
        assert!(
            Signal::Attention.status_message().contains("needed"),
            "only the attention hook may claim the user is needed",
        );
        for s in [Signal::Working, Signal::Done] {
            assert!(
                !s.status_message().contains("needed"),
                "{s:?} must not announce that the user is needed",
            );
        }
        // Copy rule: no em dashes in anything a user reads.
        for m in msgs {
            assert!(!m.contains('\u{2014}'), "em dash in user-visible text: {m}");
        }
    }

    #[test]
    fn agy_wraps_tool_events_but_not_the_others() {
        let cmds = vec![
            ("PreInvocation", "/h/pre.sh".to_string(), Signal::Working),
            ("Stop", "/h/stop.sh".to_string(), Signal::Done),
            ("PreToolUse", "/h/tool.sh".to_string(), Signal::Working),
        ];
        let out = agy_merge(&serde_json::json!({}), &cmds);
        let e = &out[AGY_HOOK_NAME];
        assert_eq!(e["enabled"], true);
        // Matcher-less events take handlers DIRECTLY. Wrapping them registers
        // an EMPTY command: visible in `agy -p "/hooks"`, and it fires nothing.
        assert_eq!(e["Stop"][0]["command"], "/h/stop.sh");
        assert_eq!(e["PreInvocation"][0]["command"], "/h/pre.sh");
        assert!(e["Stop"][0].get("hooks").is_none(), "Stop must NOT be wrapped");
        // Tool events do take the matcher group.
        assert_eq!(e["PreToolUse"][0]["hooks"][0]["command"], "/h/tool.sh");
        assert_eq!(e["PreToolUse"][0]["matcher"], "*");
    }

    #[test]
    fn agy_removal_is_a_delete_of_one_named_key() {
        let before = serde_json::json!({ "someone-elses-hook": { "enabled": true } });
        let after = agy_merge(&before, &[("Stop", "/h/stop.sh".to_string(), Signal::Done)]);
        assert!(after.get(AGY_HOOK_NAME).is_some());
        let back = agy_unmerge(&after).expect("ours to remove");
        assert_eq!(back, before, "another author's hook survives untouched");
        assert!(agy_unmerge(&before).is_none(), "nothing of ours, nothing to do");
    }

    #[test]
    fn agy_and_grok_both_write_to_the_pty_with_their_own_sequences() {
        let done = script_body("agy", Signal::Done);
        assert!(done.contains("$TERMIC_PTY"));
        assert!(done.contains("133;D"), "hard done, no settle wait");
        let working = script_body("agy", Signal::Working);
        assert!(working.contains("133;C"));
        // agy is not grok and must not carry grok's provenance gate.
        assert!(!done.contains("GROK_HOOK_EVENT"));
        assert!(done.contains(r#"[ -n "$TERMIC_TASK_ID" ] || exit 0"#));
    }

    // ── opencode ────────────────────────────────────────────────────
    #[test]
    fn opencode_is_a_plugin_not_a_config_merge() {
        assert_eq!(schema_for("opencode"), Schema::OpencodePlugin);
        // The documented plural ONLY. `.opencode/plugin` and
        // `.opencode/plugins` are both loaded, and writing both double-fires
        // every event (measured).
        assert_eq!(settings_rel("opencode"), "plugins/termic.js");
        assert!(!settings_rel("opencode").contains("plugin/"));
    }

    #[test]
    fn opencode_reports_all_four_edges_including_the_release() {
        let h = hooks_for("opencode");
        assert!(h.iter().any(|(e, s)| *e == "permission.asked" && *s == Signal::Attention));
        // The edge no other agent has: attention CLEARED, rather than inferred
        // from the next busy signal.
        assert!(h.iter().any(|(e, s)| *e == "permission.replied" && *s == Signal::Working));
        assert!(h.iter().any(|(e, s)| *e == "session.idle" && *s == Signal::Done));
        assert!(h.iter().any(|(e, s)| *e == "chat.message" && *s == Signal::Working));
    }

    #[test]
    fn the_opencode_plugin_can_never_throw_into_its_host() {
        let js = opencode_plugin_body();
        // In-process: no timeout, no exit code, and a throw in a tool handler
        // blocks the tool. Every handler must be wrapped.
        assert!(js.matches("try {").count() >= 3, "every handler needs its own try");
        assert!(js.contains("catch"), "and a catch that swallows");
        // Silent outside a termic pty, same rule as the shell scripts.
        assert!(js.contains("TERMIC_PTY") && js.contains("TERMIC_TASK_ID"));
        assert!(js.contains(&Signal::Attention.payload()));
        assert!(js.contains("133;C") && js.contains("133;D"));
        // No raw control bytes in a generated source file.
        assert!(!js.contains('\u{1b}') && !js.contains('\u{7}'));
    }

    #[test]
    fn read_settings_refuses_malformed_json_rather_than_replacing_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, b"{ not json").unwrap();
        assert!(read_settings(&p).is_err());
        // Missing and empty both mean "nothing yet", not an error.
        std::fs::write(&p, b"").unwrap();
        assert_eq!(read_settings(&p).unwrap(), serde_json::json!({}));
        assert_eq!(
            read_settings(&dir.path().join("nope.json")).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn write_atomic_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        write_atomic(&p, b"{}\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{}\n");
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("termic-tmp"))
            .collect();
        assert!(strays.is_empty(), "temp file left behind");
    }
}

