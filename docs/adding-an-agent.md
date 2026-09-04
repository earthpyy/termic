# Adding a built-in agent

A checklist, written after adding pi, opencode, codex hooks and Muse Code, because
every one of those shipped with something missed that only turned up in use. The
order matters: the measuring comes first, and half the entries below exist
because a default was written from a CLI's `--help` and was wrong.

Two rules run through all of it.

**Measure, never infer.** `--help` describes intent; the binary decides. Every
default here has been wrong at least once for an agent whose help text said
otherwise. Where a claim can be checked with a live binary, check it, and put
what you saw in the comment: the version, and what the run actually printed.

**Prefer a failure that is loud.** An agent that refuses to start gets fixed the
day it ships. An agent that starts and silently stops reporting gets found weeks
later, by a user, from a symptom three layers away.

## 0. Install it, and find its offline mode first

Before anything else, find out whether the agent can run WITHOUT an account.
It decides how much of the rest you can verify at zero cost, and it is usually
undocumented:

- muse: `--provider echo`, a real offline provider. Everything in this document
  except its attention state was measured with it.
- codex: no offline provider, but `codex app-server` answers protocol requests
  (`hooks/list`) without a turn, and a bad `model_provider` fails after the
  session has started, which is enough to observe startup behaviour.

Where there is no offline mode, budget real turns and say so before spending
them: a maintainer on the cheapest plan has very few.

## 1. The registry entry (`default_agents()` in `lib.rs`)

`id`, `display_name`, `command`, `icon_id`, `color`, and:

- **`yolo_args`** — measured, not assumed. Empty is a legitimate ANSWER (pi asks
  for no approval at all), so write the comment that says it was checked rather
  than leaving a reader unsure whether it was skipped.
- **`resume_args`** — the cwd-based resume. Right for a worktree; see §2.
- **`session_id_args` / `resume_id_args`** — see §2. Getting this wrong is
  invisible until two tasks share a conversation.
- **`sandbox_allowed_paths`** — see §4. These grant **`file-write*`**.
- **`signals`** — leave empty unless you have CAPTURED titles (§3).

Also: `agent_dirs::state_dirs`, `docker::KNOWN_SAFE_AGENTS` and
`docker::base_agent_id_str`'s `BUILTINS`, the TS `BUILTIN_FALLBACK` in
`lib/agents.ts` (the two tables MUST agree; the TS one runs before the registry
loads), `CliIcon`'s `case`, `CLI_BRAND_COLOR`, `CLI_LABEL`, `index.css` brand
vars for both themes, and a line in `Dockerfile.default`.

An existing install picks the new agent up through `load_settings_inner`'s
merge, so no migration is needed. It also means every user gets the row.

## 2. Resume: three shapes, and how to tell which one you have

The question is only interesting for a REPO-ROOT task, because several of those
share one cwd, so "resume the last session here" is another task's conversation.

1. **Mint** (claude, grok, pi). Termic can hand the agent a uuid at launch, so
   it owns the session. Both `session_id_args` and `resume_id_args`.
2. **Capture** (opencode, codex, muse). The agent will resume an id but will not
   accept one at launch. `resume_id_args` only, plus a way to learn the id after
   the fact: `post_launch_capture` (a shell command, opencode and muse) or the
   agent's own hook reporting it (codex, which is better, see §5).
3. **Neither** (agy). `resume_args` only; repo-root tasks start fresh.

**Test for the mint shape properly**, because two agents looked like it and were
not:

```sh
<agent> resume 11111111-1111-4111-8111-111111111111   # a uuid that does not exist
```

pi CREATES it (so one flag serves both mint and resume). codex answers
"no rollout found for thread id" and muse's TUI rejects `--session-id` outright
as an `exec`-only flag. Then prove resume actually carries HISTORY, not just
that it exits 0: ask the agent to remember a word, resume, ask for the word.

## 3. Work-state signals

Capture real titles before writing a pattern. `script` will not do: an agent TUI
that queries the terminal (DSR/CPR) hangs against it. Use tmux, which answers,
and read `#{pane_title}`; use `tmux pipe-pane` for the raw byte stream when you
need to know which OSC ids it emits.

Leave `attention` EMPTY unless you have captured the blocked state. A pattern
written from reasoning is what c35d297 had to remove from two agents.

**Check whether the agent puts prose on `OSC 9`.** Codex sends its ENTIRE final
message there at the end of every turn, and termic reads `OSC 9` as "the agent
wants you", so every completion became a needs-you bell. If it does that, it
belongs in `NOTIFY_NEVER_ATTENTION` — and confirm the negative too, by driving it
to a real permission prompt and checking no `OSC 9` appears.

## 4. The sandbox, which is where the loud failures live

`Agent.sandbox_allowed_paths` grants **read AND write** on a `subpath`. Two
consequences, both learned the hard way:

- **Never list a SHARED directory.** Muse's entry had `~/.local/bin`, which is
  where claude, codex, agy and grok also keep a binary or shim: a sandboxed muse
  could have overwritten any of them. Use a `regex:` scoped to the agent's own
  files instead (claude's sidecar regex is the precedent).
- **Never list another agent's config.** Some agents read their neighbours'
  personal rules; that is theirs to do uncaged, not termic's to grant.

**List every state dir, including the macOS-native one.** Agents commonly use
`~/.config/<a>` AND `~/Library/Application Support/<A>`. Muse shipped without the
second and failed to start under ENFORCING, because the missing access was a
WRITE (`session-name-authority/session-names.db`).

### Debugging a cage failure

```sh
DUMP_AGENT=<id> DUMP_PATH=<a worktree> DUMP_OUT=/tmp/a.sb \
  cargo test --lib profile_dump -- --ignored --nocapture
cd <the worktree> && sandbox-exec -f /tmp/a.sb <agent> ...
```

with, in another shell:

```sh
log stream --predicate 'eventMessage CONTAINS "Sandbox:" AND eventMessage CONTAINS "deny"' --style compact
```

Three things that will waste your time otherwise. The system log **dedupes**
violations, so each run usually reveals ONE new path and you iterate. Bisect from
the working side (`(allow file-read*)` appended, then narrow) rather than adding
denied paths one at a time. And a path may only work as a broad `subpath` even
when every child is listed individually, because of macOS firmlinks
(`/Library` is really `/System/Volumes/Data/Library`) — that is what `/Library`
being a read root exists for.

Check the Sandbox dialog's monitoring mode too. It found muse's missing
`Application Support` dir immediately, listed with its access counts, which is
faster than any of the above.

## 5. Hooks (optional, and check the transport FIRST)

Hooks are only worth wiring where the terminal gets a state WRONG or cannot
express it. Before designing anything, check the two things that make it
possible at all:

- **Does the agent pass `$TERMIC_PTY` and `$TERMIC_TASK_ID` through to a hook
  command?** Muse does not. It strips them (`HOME` survives, custom vars do
  not), and `shell_environment_policy` does not change it, so every generated
  script would exit 0 having written nothing. That killed muse hooks outright.
- **Does the readiness event fire at STARTUP?** Muse's `SessionStart` fires on
  the first PROMPT, despite its payload saying `source: "startup"`, so it cannot
  gate readiness.

Then the shape: `hooks_for`, `schema_for` (`ClaudeCompatible` covers most,
including codex), `settings_rel`, `SUPPORTED`, and whether the agent needs a
required field in a config termic creates from scratch (muse rejects a
`settings.json` with no `schema_version`).

**There is no UI step.** Settings → Agents' hooks row and the welcome wizard's
list are both driven by `SUPPORTED` crossed with what is on PATH, so adding the
id there is what makes the agent appear. The corollary is the part that looks
like a bug and is not: an agent deliberately left out shows NOTHING in that
dropdown rather than a row explaining why. If you decide against hooks for an
agent, the reasoning goes in `docs/agent-hooks.md` — that is the only place
anyone will find it, and "why is muse missing from the hooks list" is a
question that has now been asked.

**Watch for a trust model.** Codex discovers hooks, reports them `enabled`, and
does not RUN them until a `trusted_hash` entry exists in its `config.toml` —
with no error, no log and no output in any failing state. If the agent has one,
ask the agent for the hash rather than computing it, or a patch release silently
turns every hook off.

Bump `SCHEMA_VERSION` whenever a script BODY changes, or existing installs keep
the old scripts forever. There is a test that fails if you forget.

## 6. Tests and docs

- Rust: the seeded-default test (assert the flags AND the reasoning, including
  what is deliberately EMPTY).
- TS: `agents.test.ts` for spawn-arg composition; keep `BUILTIN_FALLBACK` in
  step with the Rust table.
- **Grep the whole test suite for agents used as EXAMPLES.** Giving codex
  `resume_id_args` broke `cli.e2e.ts`, which used codex as its example of an
  agent that cannot resume by id, and it was caught by CI on main rather than
  locally. Run the FULL `make e2e`, not the two specs you touched.
- Docs: `docs/sandbox.md` (vendor hosts, the Docker agent list),
  `docs/agent-hooks.md` if hooks were considered — including if they were
  REJECTED, with the measurement, so nobody repeats the investigation.
- README's built-in list.
- Do NOT touch `CHANGELOG.md`; that is the maintainer's.

## 7. Before saying it works

The suites do not catch what this feature class gets wrong. Run the agent by
hand in a worktree AND in a repo-root task, with the sandbox both off and
enforcing, and check: it starts, the spinner tracks a real turn, a completed
turn produces ONE notification, resume brings the conversation back, and the
Docker image still builds.
