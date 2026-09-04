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

Codex is deliberately excluded: its title already says `Action Required` at
+22ms, so it stays on the fallback path.

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
