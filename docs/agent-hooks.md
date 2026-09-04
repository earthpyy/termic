# Agent hooks

How termic learns what an agent is doing from the agent itself, instead of
reading its terminal. Off by default, per agent, installed globally into that
agent's own config. Settings → Agents & Terminals → Agent hooks.

The state machine this feeds (`working` / `attention` / `done`, the settle
timer, the demoters) lives in `TerminalPane`; this doc covers the hook half and
the measurements that decided it. Generation is
`src-tauri/src/agent_hooks.rs`, the OSC contract is `src/lib/agentHooks.ts`.

## Why not read the terminal

The fallback path infers state from the title (OSC 0/2), from OSC 9
notifications and OSC 9;4 progress where an agent sends them, and otherwise
from heuristics: 4s of PTY silence, a stable scrollback, an unchanged viewport
hash. It is usually right. Two measurements say "usually" is not enough:

- **Blocked reads as finished.** Claude paints its IDLE glyph while blocked on
  a permission prompt, a question, or plan approval (measured on 2.1.250). The
  false `done` fires at `SETTLE_MS`, about a second before the honest OSC 9
  arrives.
- **Running reads as finished.** Over an 8.5 minute run with four real
  subagents, the title claimed idle for **154s, 30% of the window**, while
  claude's own `Stop` correctly stayed silent throughout. This is the
  "long subagent run reports done early" bug.

The screen-text guard meant to catch the second matched **twice** in those 8.5
minutes: claude had changed its wording. A text heuristic aimed at a moving
target.

## What is installed, per agent

| agent | ready | working | attention | done | interrupt |
| --- | --- | --- | --- | --- | --- |
| claude | `SessionStart` | `UserPromptSubmit`, `PreToolUse` | `PermissionRequest` | `Stop`, guarded on the `background_tasks` whitelist | none exists |
| grok | not measured | `UserPromptSubmit`, `PreToolUse` | `Notification` | `Stop` + `StopCancelled` | `StopCancelled` |
| agy | not measured | `PreInvocation` | none exists (see below) | `Stop`, guarded on `fullyIdle` | none exists |
| opencode | not measured | `chat.message`, `permission.replied` | `permission.asked` | `session.idle` | `session.idle`, on the SECOND escape |
| codex | `SessionStart` | `UserPromptSubmit`, `PreToolUse` | `PermissionRequest` | `Stop` | none exists |

**Ready is the only signal that is not a correction.** Everything else here
replaces a state the terminal reports WRONG. Ready reports one the terminal
cannot express at all: whether a typed message will reach the agent's input
box. A blocking startup dialog paints and then goes quiet, which is
byte-for-byte what a waiting input box looks like, so every quiet-based
heuristic calls it ready.

The measured case is claude's trust picker: a repo claude has never run in gets
`Is this a project you created or one you trust?` with `No, exit` HIGHLIGHTED,
and no hook fires while it is up, not even `SessionStart`.

Trust resolves through the REPO, not the directory. A worktree of an
already-trusted repo inherits it and is given no record of its own, so this is
NOT an every-task event: it is the first task in a project just added to
termic, or on a machine where claude only ever runs through termic. Measured
both ways (standalone `git init` prompts, `git worktree add` off a trusted repo
does not) after the opposite was assumed and written down here. termic's first-message path waited out its 3s floor, saw
quiet, typed the prompt into the picker and sent the submit 450ms later, which
confirmed the highlighted option: the agent EXITED. One injection, no retry
needed. That is why a retry loop is not a fix on its own here, and why
`seedPromptWhenReady` refuses to type at all when an agent that reports
readiness has not reported it (`lib/agentReady`, outcome `blocked`).

Ready reuses OSC 777 `notify` with the trusted `termic` title, and is told
apart from attention by its BODY alone (`HOOK_OSC_READY_BODY` in
`lib/agentHooks.ts`, pinned on both sides). No new OSC id to parse, and it
inherits the v3 three-target write that makes hooks work inside Docker.

Only claude registers it. The other agents have plausible-looking candidate
events, but a guessed event name installs a hook that never fires and is
indistinguishable from one that does. They keep the echo fallback below.

**Agents without hooks are not left on the old behaviour.** `deliverMessage`
takes a `verifyEcho` option, used by the first-message path only: it writes the
text, watches the PTY for the agent echoing it back, and sends the submit only
if it does. An input box echoes what you type; a selection list does not. That
is a discriminator rather than another timer, and it is what protects an agent
whose hooks are not installed. The queue does NOT use it: a queued message is
only ever sent after a turn ended, which already proves the agent was at its
input box.

Codex is excluded from the ECHO fallback specifically, and only that: its title
already says `Action Required` at +22ms, so a first message typed into it does
not need the echo check. It is a full hooks agent otherwise (see below).

**Muse Code cannot use this transport, and that is measured rather than
assumed.** It has a hooks system and it looks like an easy win: the config is
claude-shaped, lives at `~/.config/muse/settings.json` (needs
`"schema_version": 1`, or muse rejects the whole file), and `SessionStart`,
`UserPromptSubmit`, `Stop`, `PreLLMCall` and `PostLLMCall` all fire, every
payload carrying `session_id`. Two things kill it, both checked against a live
1.0.2 through its offline `--provider echo`:

- **Muse strips the environment it hands a hook.** `HOME` survives;
  `TERMIC_PTY`, `TERMIC_TASK_ID` and an arbitrary probe variable all arrive
  EMPTY. Every script here opens with `[ -n "$TERMIC_TASK_ID" ] || exit 0` and
  writes to `$TERMIC_PTY`, so all of them would exit 0 having written nothing,
  silently. Setting `shell_environment_policy` (`inherit: all`,
  `include_only: ["TERMIC_.*"]`) does not change it, so that policy governs the
  agent's own shell tool and not hook commands. The Docker fallbacks do not
  rescue it either: `/proc/1/fd/1` is Linux-only and `/dev/tty` fails for a hook
  with no controlling terminal.
- **`SessionStart` is lazy.** It does not fire at startup; it fires on the FIRST
  PROMPT. Nothing had arrived 12s into an idle session, and it landed the moment
  a prompt was submitted. Its own payload says `source: "startup"`, which makes
  this easy to get backwards. So it could not be the readiness gate that earns
  claude's `Ready` signal even if the transport worked.

None of which is urgent, because muse is the rare agent whose TITLE already
carries real state: a braille spinner prefix while the model runs, the bare
workspace basename when idle, both captured off a live PTY. The title path
classifies it unaided, and unlike codex it never puts its final message on an
`OSC 9` (a full capture shows `OSC 0`, `4`, `10` and `11` only), so it has none
of the end-of-turn bell problem either. What hooks would add is the attention
state, which is the one thing its title was not measured for.

Wiring it would need a transport muse cannot strip, e.g. baking the pty path
into the command at install time, which the file-based installer cannot do
because that path changes per spawn.

**Muse Code is also the case the echo guard cannot cover, and the reason
`UNATTENDED_SPAWN_ARGS` grew a third entry.** Muse has a trust picker too
(`Do you trust this workspace?`, options `1 Trust and continue` / `2 Quit`),
but unlike claude's it is keyed by DIRECTORY rather than by repo, so it is a
genuinely every-task event: measured, a `git worktree add` off an
already-trusted repo prompts again. The part that defeats the guard is which
keys the picker binds. `n` selects `Quit` and a SPACE confirms it, so the
prompt "rename the button" exits the agent partway through being typed, with
no Enter involved at all. The guard withholds the submit, which is the wrong
end: by then the damage is done. Measured both ways on a live 1.0.2, no Enter
sent in any of them: `hello world` and `add a test` leave the picker up, while
`rename the button` and a bare `n` then space exit.

So the unattended path passes `--trust-workspace`, muse's own flag for this.
It trusts for THAT RUN ONLY (verified: the same directory prompts again on the
next spawn without the flag), so an unattended launch cannot quietly leave a
workspace trusted afterwards, and an attended spawn still gets the picker and
answers it itself. A caged task never sees any of this: `isTaskCaged(task) ||
task.yolo` means it always gets `--yolo`, which trusts the workspace for the
run as a side effect of dropping approval.

**`PreToolUse` is a heartbeat, not a duplicate.** Working is the only sustained
state here; every other signal is an edge. The title re-asserted working on
every repaint, so anything that wrongly cleared the spinner self-healed within
a frame. A single `UserPromptSubmit` made the same clear permanent for the rest
of the turn: a user clicking into a running task had its spinner dropped and
nothing ever put it back. A tool call is the protocol's own "still going".

agy is the exception twice over: `PreInvocation` already fires per model
invocation, and its `PreToolUse` documents `decision` as REQUIRED, so observing
it silently risks blocking the tool.

**Done and attention get no heartbeat.** A turn ends once, and repeating it
would re-badge something the user just dismissed. There is a test saying so.

## claude's attention body names the tool, because that body is the one you read

The hook wins the race and therefore composes the banner. Measured: it fires the
instant claude blocks, and claude's own `OSC 9` ("Claude needs your permission")
arrives a further **6.0s** behind it, by which point the notification has been
delivered and cannot be edited. Raising a second one for the same prompt is the
GH #276 bug, so the agent's own wording only ever reaches the badge.

So the hook says the more useful thing, since it knows something claude's own
notification does not mention: which tool is being asked about. The body is
`needs your permission: Bash`, or `needs your permission: mcp__github__create_pr`,
which is the case that earns the feature (a bare "needs your permission" tells
you nothing when six MCP servers could be asking).

`tool_name` is claude's documented field for the tool events, `PermissionRequest`
included, read out of 2.1.259's own embedded hooks reference (`session_id`,
`tool_name`, `tool_input`). The script extracts the FIRST occurrence, the
opposite of the Done guard's `##`, because that serialisation order puts the real
field ahead of `tool_input`. Still no `jq`, and still exit 0 on every path: a
payload it cannot read falls back to the generic `HOOK_OSC_BODY`.

Two guards, both pinned by tests that RUN the generated script rather than
grep it:

- **A tool name must be a bare identifier or it is rejected outright**, not
  sanitised. The body sits in OSC 777's LAST field, so an injected `;` would
  re-point the earlier fields, and a body of `termic;...` landing in the title
  position is exactly how a payload would forge a trusted signal.
- **The body goes through `printf`'s `%s`, never into its format string.** A `%`
  cannot reach a format specifier even if a future tool name gets past the
  identifier check.

One surprise worth knowing before you debug it: whitespace inside a tool name is
not rejected, it is deleted, because the payload is flattened with
`tr -d '[:space:]'` first (the same step the Done guard uses). `Bash Write` reads
as `BashWrite`. Harmless, since the extraction stops at the closing quote and can
never produce a field separator, and asserted so nobody has to work it out twice.

**The badge keeps the FIRST wording, not the last.** claude's later `OSC 9` marks
attention again, and letting it overwrite would leave the badge vaguer than the
banner the user already read, and disagreeing with it. That second mark is also
flagged `repeat` (see `Tab.unread.repeat`), so its silence is deliberate rather
than an accident of the rising edge being spent: without the flag, anything that
cleared `unread` between the two marks produced two banners for one prompt.

The echo is bounded by TIME (`ATTENTION_ECHO_MS`, 10s), not by "a needs-you mark
is already held". That was the first shape and it had a hole worth not
rediscovering: a permission prompt is answered with a BARE KEY (`y`), and only
Enter clears the mark, so the state test stayed true long after the user had
dealt with the prompt and went silent on the NEXT genuine needs-you. A re-ask
inside the window can only happen with the user at the keyboard, where the focus
gate suppresses the banner anyway.

## codex hooks do not run until they are TRUSTED

Codex was left out of this feature at first, on the grounds that it "does not
need it": its title reports `Action Required` at +22ms, so attention was already
covered. That argument was about the wrong state. An agent with no hooks can
never reach the `hooksOwn` branch, so every turn it runs is ended by a guess,
and the byte-quiet fallback calls a turn done after 4s of silence. Four seconds
of silence is an ordinary model round-trip. That is GH #276, and codex is the
agent it was reported on.

The event names are claude's, exactly. Codex 0.153.0 reads a claude-shaped
`hooks.json` and its own `HookEventName` enum carries the same set, so the
mapping and the `Schema::ClaudeCompatible` merge are reused unchanged. The file
is `$CODEX_HOME/hooks.json` (flat, NOT `hooks/hooks.json`), confirmed by codex
reporting a hook written there with `source: "user"`.

**What is new is trust, and it fails silently.** Codex discovers the file,
reports every hook in it as `enabled: true`, and then does not run them.
Measured, all four states:

| config | `trustStatus` | runs? |
| --- | --- | --- |
| `hooks.json` alone | `untrusted` | no |
| plus `enabled = true` | `untrusted` | no |
| plus a WRONG `trusted_hash` | `modified` | no |
| plus the right `trusted_hash` | `trusted` | **yes** |

There is no error, no log line and no output in any of the three failing rows.
An install that stopped after writing `hooks.json` would look exactly like a
working one and report nothing, forever. So installing codex hooks writes two
files: the hooks, and a trust entry per hook in `$CODEX_HOME/config.toml`.

```toml
[hooks.state."/Users/u/.codex/hooks.json:session_start:0:0"]
enabled = true
trusted_hash = "sha256:…"
```

The key is `<sourcePath>:<snake_case_event>:<group>:<handler>`.

**The hash is asked for, never computed.** It is not the sha256 of the command,
the handler object or the file (all three checked, plus six TOML shapes) but
codex's own serialisation of an internal identity struct. Reimplementing that
would mean a codex patch release flipping every hook to `modified` with no
symptom other than an agent that stops reporting. So `codex_trust.rs` asks
codex: one `initialize` + one `hooks/list` over `codex app-server`, which is
part of its generated protocol schema rather than a private detail, and it
returns the key and the current hash. Removal needs no such call, because the
key begins with the hooks file's path.

**Paths are compared canonicalised.** Codex reports the path it RESOLVED, and on
macOS both likely scratch homes (`/tmp`, `/var/folders/...`) are symlinks under
`/private`. A plain `==` rejects termic's own file as somebody else's and the
install fails with "codex did not report the hooks termic just wrote". Found
exactly that way.

**Ownership is decided twice**, by the command's `termic-hooks/` prefix AND by
the file the hook came from. A hook whose command merely looks like ours, in a
project config someone copied from a dotfiles repo, is a stranger's script;
trusting it on prefix alone would be termic approving arbitrary code on the
user's behalf.

**Docker writes nothing at all, and succeeds.** Inside a container both halves of
the trust entry are unknowable from outside: the key would need the container's
path (`/root/.codex/hooks.json`), and the app-server that reports the hash runs
in a container that does not exist at install time. Writing hooks that are
discovered and silently never run is the exact failure this feature removes, so
the Docker half writes nothing and `status()` reports it off, which is true.
Returning `Ok` matters as much as writing nothing: `agent_hooks_install` rolls
the HOST install back when the Docker half errors, so refusing there made codex
hooks uninstallable everywhere while every direct-install test still passed.

**Two ignored tests cover this, and they need a real codex.**

```sh
cargo test --features e2e codex_hooks_install -- --ignored --nocapture  # install/trust/remove
cargo test --features e2e codex_hooks_fire    -- --ignored --nocapture  # a live turn
```

The first asserts codex's own verdict rather than the file termic wrote, since a
trust entry with a wrong hash is silently `modified`. The second runs a real
turn and reads a real pty, and captured exactly this:

```text
ESC]777;notify;termic;agent ready for input BEL   SessionStart
ESC]133;C BEL                                     UserPromptSubmit
ESC]133;D BEL                                     Stop
```

It borrows the user's login with a SYMLINK to `auth.json` rather than a copy, so
no credential is duplicated into a temp dir, and it never writes to the real
`~/.codex`. It uses a real pty deliberately: the scripts write with a truncating
redirect, which is meaningless on a character device and total on a regular
file, so pointing `TERMIC_PTY` at a file makes a three-hook turn look like a
one-hook turn. That cost an hour once.

`PermissionRequest` is the one signal not proven by those two, because
`codex exec` pins approvals to `never` and never reaches a prompt. It was
measured separately by driving the real TUI to an approval prompt: the hook
fired the instant the prompt painted, carrying `"tool_name":"Bash"`, which is
what the shared attention body turns into `needs your permission: Bash`.

One interaction worth knowing: codex's title ALSO reports `Action Required`, so a
permission prompt now marks attention twice, once from each source. That is the
same shape as claude's hook-then-`OSC 9` pair and it is handled by the same
`ATTENTION_ECHO_MS` window, so it costs one banner, not two.

## agy has no attention event, so it is read from the screen

Measured against Antigravity CLI 1.1.24 at a live permission prompt: **no
title, no OSC of any kind, no bell.** The prompt exists only as text:

```
Requesting permission for:
   echo hello-from-agy
Do you want to proceed?
```

So agy is a hybrid: hooks for working and done, screen content for attention.
`BUILTIN_OUTPUT_SIGNALS` in `lib/agents.ts` is a table SEPARATE from the title
one, deliberately: claude's `^\s*✳` describes a title and is nonsense against
stdout, which is why the output scanner refuses to fall back to it.

Give a pty a window size before probing a TUI. Two earlier probes concluded agy
emits nothing at all, and both were invalid: without `TIOCSWINSZ` agy never
finishes booting, so the probe was typing into a splash screen.

## Transport

Every agent writes one OSC to the terminal termic handed it. Three targets,
tried in order, chained on redirection failure:

```sh
emit "$TERMIC_PTY" || emit /proc/1/fd/1 || emit /dev/tty || true
```

- `$TERMIC_PTY` is the slave path from `ptsname(master_fd)`, exported per spawn.
- `/proc/1/fd/1` is the container's main-process stdout, which docker relays to
  that same pty under `run -i -t`. It needs NO controlling terminal, which is
  why it beats `/dev/tty`: hooks run without one (measured on grok, rc=1).
- Chained on failure rather than a readiness test, because `[ -w /dev/tty ]` is
  true in places where opening it fails. Attempting the write IS the test.

**Docker needs the env, not just the script.** `cmd.env(...)` sets the HOST
process's environment, and for a Docker task that process is the `docker run`
CLI. Docker forwards none of it, so `TERMIC_PTY` and `TERMIC_TASK_ID` are
pushed onto the container env in `docker::build_spec`, with `TERMIC_PTY` set to
the CONTAINER's address (`/proc/1/fd/1`). Before that, every hook in a
sandboxed tab exited on its first line and a whole tab delivered zero OSCs
while its unsandboxed neighbours delivered hundreds.

**Why not `terminalSequence`.** claude can write the OSC itself, and its
runtime allowlists that to OSC 0/1/2/9/99/777 plus BEL. OSC 133 is not on the
list and is dropped SILENTLY, so a Done sent that way never arrives. Measured.

**Why not the CLI socket.** `cli_server` is a command surface whose token is
deliberately kept out of the app environment, because `pty_spawn` copies that
environment into every child: an env-stashed token would be a sandbox escape.
The seatbelt profile also denies caged agents that socket by path, and a
container has neither the socket nor the binary. An ingest-only socket (no
verbs, no shared secret) would be a sound escalation if the pty ever proves
insufficient; the cage already permits unix sockets generally.

## Hooks own the turn, once they have proved it

While an agent's hooks are installed AND at least one has been seen on that
pty, nothing else may end a turn for it: not the title, not byte-quiet, not
scrollback stability, not the settled hash, not the 90s ceiling. They are one
heuristic wearing five hats, and leaving any armed reproduces the bug hooks
exist to fix.

**The 10-minute absolute ceiling is the one exception, and it must stay one.**
It is not a sixth guess at whether a turn ended; it is a liveness backstop, and
it ran BELOW the hooks-own gate until an artifact watch proved what that costs
(see below). Everything termic knows about a hook-owned turn arrives over a
contract termic does not control, so the state machine needs one bound that
does not depend on that contract holding. Ten minutes of a genuinely working
agent costs a spinner that clears early and is re-armed by the very next
`PreToolUse` heartbeat. A missing done costs the tab, permanently.

**Installed is not working**, and conflating them hangs the UI. Hooks earn the
right per pty by delivering once (`hookSeenRef`). A working hook proves itself
on the first submit; a transport that cannot deliver leaves the fallbacks armed,
which is the behaviour that existed before hooks. That is what stopped Docker
tabs pinning to `working` forever while their hooks wrote into nothing.

The title must not drive `working` either, once hooks own the tab. Captured
live: a hook reported `133;C` then `133;D` 1ms apart, the title re-armed
`working` 19ms later, and the next genuine `133;D` was swallowed by the
one-done-per-submit token the first had spent. The tab claimed to be working
for 44 seconds with the agent idle at its prompt.

**A hook done outranks that token.** The token stops one turn NOTIFYING twice
(a settle, a late OSC 9); it was never meant to stop a turn ENDING. Safe by
measurement: claude fires `Stop` several times per turn and the script drops
every one whose `background_tasks` holds delegated work, so at most one
qualifying done reaches us.

## The done guard is a whitelist, because "in flight" includes things that never end

claude's `Stop` payload carries `background_tasks`, and the guard used to hold
the turn open whenever that array was non-empty. That question is the wrong one,
and a session that publishes an artifact answers it wrong forever.

Read out of 2.1.259: the payload is built by mapping the WHOLE task registry
through a single filter, `status is running|pending && isBackgrounded !== false`.
Nothing else is dropped, and the switch that decorates each entry has an
explicit arm for `monitor_ws`, a websocket monitor. An artifact's live-updates
subscription is one of those, it opens on the first publish, and it stays
`running` for the rest of the session. claude's own task list hides exactly
these (`if (t.type === "monitor_ws" && t.ambient) continue`); the hook payload
does not.

So from one `Artifact` publish onward, every `Stop` looked like outstanding
work, the hook emitted nothing, and because hooks had already proved themselves
on that pty there was no demoter and no ceiling left to notice. Reported as a
tab stuck on loading across new turns and finished turns, which is exactly what
it was.

The guard now asks whether the AGENT is still working, which only delegated
work answers. Held: `subagent`, `workflow`, `shell`, `teammate`, `cloud
session`, `MCP task`. Not held: `monitor` (either dialect), `dream`, `auto-mode
scan`, and any type a future release adds. The friendly labels are claude's own
(`local_agent` → `subagent`, `local_bash` → `shell`, and so on); an unrecognised
type falls through to its raw discriminant and therefore to done.

**Fail towards done here, the opposite way to the fallback path.** Once hooks
own a tab, a wrong hold is permanent and a wrong done is corrected by the next
`PreToolUse` heartbeat, because working is the only sustained state and it is
re-asserted many times per turn. That asymmetry is why an unknown type emits.

The array is sliced out of the payload before matching, not matched across the
whole thing: `last_assistant_message` carries the agent's own prose, and a turn
that discussed `"type":"shell"` would otherwise hold its own spinner down. `##`
(longest prefix) picks the last occurrence, which is the real field, since
claude serialises the message first. `agent_hooks.rs`'s tests run the generated
script under `/bin/sh` against real payload shapes rather than asserting on its
source, because every bug this guard has had was one a substring assertion
agreed with.

## Interrupts: the one thing hooks do not cover

Measured on every agent, both keys, mid-generation:

| agent | Escape | Ctrl-C |
| --- | --- | --- |
| claude | no hook; idle glyph 110ms later | no hook; idle glyph 40ms later |
| grok | `StopCancelled`, `reason="user_interrupt"` | same |
| agy | nothing, and no title either | nothing |
| opencode | first press does nothing; the SECOND fires `session.idle` | `session.idle`, then it exits |

claude was measured with 29 of its 31 lifecycle events registered; its event
list, read out of the binary, has no cancel or abort event to register.

So the keystroke has to be part of the evidence, and can never be all of it:
opencode's first Escape leaves it streaming, so acting on one press would end a
live turn. termic accepts an interrupt only when the key is corroborated by the
title going idle (3s) or the terminal falling quiet (15s), and only while the
user is watching. It calls `interruptWork`, not `fireDone`: an interrupt is not
a completion, so it earns no badge and spends no done token.

## Config shapes, and the grok guard

Three shapes: claude-compatible JSON (claude, grok), Antigravity's named-block
shape with tool events grouped under a matcher and the rest direct (agy), and a
generated JS plugin (opencode). Paths are in `settings_rel`; agy's LIVE path is
`config/hooks.json` (`antigravity-cli/hooks.json` parses, logs "loaded 1 named
hooks", and executes nothing).

grok reads `~/.claude/settings.json` on purpose, so installing both double-fires
everything. termic exports `GROK_CLAUDE_HOOKS_ENABLED=false` on grok tabs, and
each script carries the opposite `GROK_HOOK_EVENT` gate.

## Versioning and auto-maintenance

`SCHEMA_VERSION` in `agent_hooks.rs`, recorded in each install's
`manifest.json`. An install from an older version is `stale`, and
`agent_hooks_sync` (run from `refreshAgentHooks` on startup) rewrites it. The
user's consent is "hooks on for this agent", not "these exact scripts": asking
someone to notice a version number and press a button is asking them to do our
job, and it fails quietly, since a stale install still reports itself installed
while reporting less than it could. v2 added the heartbeat; v3 added the
container transport, which a v2 install does not have at all.

Sync only touches agents already installed, so it never introduces hooks for
one the user declined, and re-running `install` preserves the pre-install
backup (`if !backup.exists()`), so removal stays byte-for-byte.

**It has to be CALLED, and for two releases it was not.** `agent_hooks_sync`
existed, was registered as a command, was exported as `agentHooksSync`, and
`AgentHookStatus.stale` was documented as "kept up to date automatically by
`agentHooksSync`" - and no code path invoked it. Every install therefore stayed
at whatever `SCHEMA_VERSION` it was created with, forever, reporting
`installed: true` while missing whatever a later set added. The v3 Docker fix
shipped this way: a v2 install inside a container is not quieter, it is dead,
and it would have stayed dead.

`App.tsx` now calls `syncAgentHooks()` at startup, which syncs and THEN reads
status, so `agentHooksInstalled` reflects the upgraded install rather than the
one just replaced. This is the mechanism the Ready signal depends on: without
it, only users who install hooks for the first time after this release would
get a readiness signal, and everyone already opted in would keep typing into
startup dialogs.

The consent recorded is "hooks on for this agent", not "these exact scripts".
That is what makes an unattended upgrade legitimate, and it is also its limit:
sync may replace a set, never introduce one.

**Sync gates on `ours_present`, never on `installed`, and the difference is
load-bearing.** `installed` is an ALL over the CURRENT event set, because a
partial install would otherwise report as done while missing a signal. That is
right for the UI toggle and catastrophic as an upgrade gate: the moment a new
event joins an agent's set, every existing install fails the ALL, reads as not
installed, and is skipped by the sync whose entire job is to add that event.

This was not hypothetical. Adding `SessionStart` and syncing upgraded agy and
grok, whose sets had not changed, and silently skipped claude, the one agent
the new event was for. It went unnoticed through a green unit suite because
every part was individually correct: the merge, the schema bump, the staleness
rule, the new signal. Only the INTERACTION between "what counts as installed"
and "what sync will touch" was wrong, and nothing tested a pair.

`ours_present` is ANY entry of ours, i.e. consent, which is the question an
unattended upgrade should actually be asking. It is safe as a gate because
`remove` deletes every entry AND the script directory, so an opted-out user
reads false and is never re-installed behind their back. Pinned by
`an_install_missing_a_newly_added_event_is_still_ours`.

## The status line is not a hook, and rides the same channel anyway

claude's usage feed (GH #277) is installed by the same `install` call, into the
same config file and the same script dir, and writes the same OSC 777 with the
same trusted `termic` title. It is NOT in `hooks_for`, is not registered against
an event, and `installed` must never depend on it: `status()` answers about
hooks, and a config whose statusLine slot was already taken is still a complete
hook install.

**Claude Code pipes `rate_limits` into a statusLine command's stdin on every
turn**, piggybacked on the Messages API response, so reading it costs no request
and no rate-limit budget. Measured on 2.1.260, the payload also carries
`context_window`, `cost`, `model` and `session_id`.

Three things about it were measured rather than assumed, and each one would have
killed the approach if it had gone the other way:

- **Whatever the script prints is RENDERED**, under the user's input box, every
  turn. A probe printing `ZZMARKERZZ` put `ZZMARKERZZ` on screen. A probe
  printing an empty string left no text at all, which is what makes the slot
  usable as a pure data channel: the numbers go out the side, stdout stays
  empty, and the agent looks unchanged.
- **claude does NOT strip the environment it hands a statusLine command.**
  `TERMIC_PTY`, `TERMIC_TASK_ID` and an arbitrary unrelated variable all arrive
  intact. This is the exact thing that killed the transport for Muse Code, so
  it was worth checking rather than assuming, and it is why the numbers arrive
  already attributed to a task with no cwd mapping.
- **`/dev/tty` fails the same way it does for a hook** (`Device not
  configured`). The three-target chain is load-bearing here too, not
  defensive.

**It runs about once per TURN, not several times per second.** Measured on
2.1.260 with an instrumented script counting its own invocations: 5 runs across
4 turns, and 2 runs across a single 60s streaming turn (one of them before the
prompt was even submitted). Worth stating because the opposite is a plausible
guess and Orca's own source comments claim their statusline "ticks ~3x/sec
while streaming", which would have made a spawn-per-tick script a real
regression and forced a throttle. It does not, on this version, so there is no
throttle here. If a future release changes that, this is the measurement to
redo before anything else.

One invocation costs ~12ms wall / ~11.5ms CPU, of which ~9ms is the bare
`/bin/sh` spawn and drain: the parsing itself is ~2.5ms. Against a turn
measured in seconds, per turn, that is not a number worth optimising.

**The slot is not termic's.** A config has exactly one `statusLine`, and a user
who wrote their own looks at it on every turn. `merge_statusline` claims it only
when it is free or already ours, decided by the command's path prefix, the same
ownership test the hook entries use. A slot taken by anyone else is left exactly
as it is and that user simply gets no usage. The script is still WRITTEN in that
case, so clearing their own status line later turns the feature on at the next
sync without a reinstall. Docker is unaffected: termic owns that config dir
outright, so there is no slot to lose.

**A PROJECT-level status line silently wins, and there is nothing to do about
it.** Claude Code reads `.claude/settings.json` inside the repo as well as the
user's own, and the project one outranks it. Measured with termic's real
installed status line at user level and a marker one in a project's
`.claude/settings.json`: the project's marker rendered in the TUI and termic's
usage OSC never fired at all.

So a repo that ships its own status line gets no usage chip for its tasks,
while every other project on the same account still does. That is confusing and
it is still the right behaviour: the alternative is writing termic's absolute
path into a file that lives IN THE USER'S REPOSITORY and gets committed, which
is not a trade any footer is worth. The same is true of
`.claude/settings.local.json`.

The honest gap is that termic does not currently NOTICE. Detecting it means
reading the task's repo settings per task rather than per agent, and saying so
somewhere the user will look; worth doing if this feature ships, and tracked in
docs/ideas/usage-footer.md rather than pretended away here.

Sources that are NOT this, all measured and all dead: claude's HOOK payloads
carry no usage field (a real `Stop` payload has `session_id`,
`transcript_path`, `cwd`, `prompt_id`, `permission_mode`, `effort`,
`background_tasks`, `session_crons`, and nothing else); its transcript JSONL
writes `quotaLimits` ONLY on a request that was already rejected; and its OTEL
export has no rate-limit metric. codex is the other way round entirely: it has
no status line and is ASKED, over `account/rateLimits/read` (docs/ipc.md).
docs/ideas/usage-footer.md carries that whole comparison, including the OAuth
endpoint and why the keychain makes it the wrong default on a Mac host.

## Cloned agents

Everything here resolves through `extends`: the event set, the schema, the
config file, and the config DIRECTORY. A clone made to hold a second account
relocates its whole config with the agent's own env var
(`CLAUDE_CONFIG_DIR=~/.next-claude`), so `agent_dirs::instance_config_dir`
resolves the directory from the agent ENTRY rather than the base's default.
Writing the base's path there would put one account's hooks into the other
account's config, which is worse than installing none.

Deliberately NOT `docker::base_agent_id_str`, which falls back to `"claude"`
for an unrecognised base. Right for choosing a Docker mount, wrong here.

## Diagnostics

`termic-workstate.log` in the OS temp dir (`-dev` and `-e2e` suffixes per build
flavour, so two live apps do not interleave). Always on, no flag: by the time
anyone notices a wrong badge, the evidence has to already be on disk.

It records state CHANGES and REFUSALS, because a resulting state cannot tell
you why it is wrong: "working was requested and refused because the tab is in
its post-click grace window" and "no working signal ever arrived" leave
identical state and have opposite causes. `hook-osc` lines record every hook
OSC on ARRIVAL, since the heartbeat mostly lands on a tab that is already
working and would otherwise be invisible. The spawn line carries `base`,
`inherited`, `hooksInstalled` and `hookProven`, which are the first four things
to check and the easiest to get wrong.

## Traps

- **`/dev/tty` does not work from a hook on the host.** No controlling terminal
  (measured on grok, rc=1). Opening the slave BY NAME needs none.
- **`SubagentStop` is not a done.** 107 fires against 2 real Stops in one run.
  `background_tasks` already covers subagents (`type: "subagent"` ×1006,
  `"shell"` ×1529).
- **"Is anything in flight" is not "is the agent working".** The registry holds
  ambient monitors that outlive every turn. Whitelist the types that mean the
  agent will be woken again; see the guard section above.
- **claude's notifications are not all needs-you.** Eleven `notificationType`s,
  five of which mean it. `agent_completed` sends `` `${label} finished` `` when
  a turn SUCCEEDS, and termic spoofs iTerm2 so it arrives as OSC 9. See
  `BUILTIN_NOTIFY_ATTENTION`: an allow-list, because the failure mode is a type
  the vendor adds LATER.
- **Read the binary, not the website.** Both wrong conclusions in this work
  (agy "loads hooks and never runs them", grok "has the least complete event
  set") came from reasoning instead of asking the installed binary. grok has
  the most complete set of the four.
