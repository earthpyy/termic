# Usage footer

A per-provider usage readout in the task footer: how much of the subscription's
rolling limits the currently shown account has spent. Requested in GH #277 by a
user who moved from Orca and misses it.

**A proof of concept is IN THE TREE and works.** This is still an idea, not a
plan: nobody has decided it should ship, and the open questions at the bottom
are product calls rather than engineering ones. What is built is described here
so that decision can be made against something real instead of a sketch.

Everything now TRUE OF THE APP has moved to the reference docs, which are the
ones to trust: the claude status line, its three measurements and the
slot-ownership rule are in [agent-hooks.md](../agent-hooks.md); the codex RPC
is
in [ipc.md](../ipc.md). This file keeps the comparison that chose them, because
the rejected options are the expensive part to re-derive.

## What it shows

Orca's footer reads `58% 5h - 41% wk` per logged-in provider. That is NOT
context usage, which is the natural misreading. It is the two rolling windows a
subscription enforces: the short session window and the long one. Some sources
carry context usage in the same payload, so a context readout would be nearly
free, but it is a different number and would need its own label.

## What was built

- **claude** reports itself, through a status line installed alongside the
  agent hooks (schema v7). It prints nothing and writes one OSC 777 on the
  hook channel, so the agent looks unchanged.
- **codex** is asked, over `account/rateLimits/read` on `codex app-server`.
- Both are keyed by AGENT ENTRY id, so a clone holding a second login gets its
  own number and two tasks on one clone share one.
- The chip is leftmost in the footer's right group, so it and the "N blocked"
  chip grow leftwards and the sandbox status stays pinned rightmost. It
  self-hides until an account has actually reported, so an agent with no source
  costs that row no width.

Verified live end to end: the generated status line against Claude Code 2.1.260
in a real PTY, and the RPC against a real codex 0.153.2
(`cargo test codex_rate_limits_live -- --ignored`). Three e2e cases in
`agent.e2e.ts` drive the whole chain through a real window.

## The source hunt

Every candidate was measured against the installed binaries, not reasoned
about. Three of the seven are dead, and they are recorded so nobody re-walks
them.

| source | verdict |
| --- | --- |
| claude `statusLine` stdin payload | BUILT |
| codex `account/rateLimits/read` RPC | BUILT |
| claude OAuth usage endpoint | works, but needs the keychain on a Mac host |
| codex rollout JSONL `token_count` | works, but the RPC dominates it |
| claude hook payloads | DEAD: no usage fields |
| claude transcript JSONL | DEAD: written only once already rejected |
| claude OTEL metrics | DEAD: no such metric, and needs a collector |

The three dead ones are written up in [agent-hooks.md](../agent-hooks.md),
which
is where the next person hunting for a claude signal will be standing.

The codex rollout file (`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl`, a
`token_count` event carrying `rate_limits` plus `model_context_window`) is a
real second source, kept documented as the fallback if the RPC ever goes away.
The RPC beats it on every axis: it answers cold with no agent running, it
returns `accountId` so attribution is explicit rather than inferred, it needs
no
directory walking, and it dodges the trap that `session_meta.cwd` inside Docker
is the path as the CONTAINER sees it and can never be matched against a host
worktree.

## The OAuth endpoint, and why it is not the default

`GET https://api.anthropic.com/api/oauth/usage` with the Claude Code OAuth
token, `anthropic-beta: oauth-2025-04-20` and a `claude-code/*` UA. Verified
live, HTTP 200, returning `five_hour` and `seven_day` utilisation with
`resets_at`, plus per-model weekly windows and a spend block. Kept out of the
build, for reasons worth keeping written down.

**It is not an OAuth flow.** termic would never run the authorize/PKCE
exchange and never open a browser; it would reuse the token claude already
holds. There is nothing to implement. That is the obvious first fear rather
than the real one.

**The real one is the refresh.** The stored credential carries `expiresAt`,
`refreshToken` and `refreshTokenExpiresAt`, and a live access token measured
here had 3.5 hours left. Something has to refresh it, and that something must
never be termic: if termic spends the refresh token and the server rotates it,
claude's own credential goes stale and a footer has logged the user out of
Claude Code. Read-only, always. Riding a turn-end edge would make read-only
sufficient rather than merely safe, since claude has just used that token
milliseconds earlier.

**On a Mac host it needs the keychain, and the ACL is the blocker.**
`~/.claude/.credentials.json` looks like the fallback and is a decoy: measured,
a 280-byte skeleton with the right keys and an EMPTY `accessToken`. The token
lives in the keychain and nowhere else. That keychain item's only
decrypt-authorised application is `/usr/bin/security`, claude itself included
in
what is NOT listed, so a native Security-framework read from termic.app raises
a
system dialog naming another app's secret. Shelling out to `/usr/bin/security`
sidesteps it, but rests on an ACL termic does not own, and **termic touches no
keychain today at all**.

**Docker inverts it.** A container has no keychain, so claude writes a real
credentials file into the mounted config dir, which termic already owns. Free
there and awkward on the host, which is the wrong shape for a footer.

Where it would still earn its place is the cold start below.

## Open questions

These are why this is still an idea.

- **Is the status line slot worth taking at all?** It is claimed only when
  free,
  handed back on uninstall, and prints nothing, so the cost is invisible rather
  than zero: a user who later opens `~/.claude/settings.json` finds a termic
  script in a slot they did not fill. The install preview says so in as many
  words, which may or may not be enough.
- **A project-level status line shadows ours, silently.** Measured: a repo's
  own `.claude/settings.json` statusLine outranks the user-level one termic
  installs, and termic's OSC never fires, so tasks in that project show no chip
  while every other project on the same account does. termic must not write
  into a repo's settings to win that fight, so the fix is to DETECT it and say
  so, which means reading the task's repo settings per task rather than per
  agent. Not built.
- **Cold start.** The status line only speaks while a turn runs, so the chip is
  absent until a task takes one, which is the opposite of what a footer is for.
  Today it shows nothing, and codex does not have the problem because the RPC
  answers cold. The options are to cache the last value per agent entry and
  show it with its age, or to spend one call per provider at launch, which is
  the one thing the OAuth endpoint would be good for.
- **The codex refresh cadence.** Every refresh spawns an app-server, so the
  chip asks every two minutes, for the visible task only. That is a guess,
  not a measurement. If it ever matters the honest fix is to drive it off the
  `Stop` hook termic already receives rather than to tune the interval.
- **Scope.** claude and codex only. The other agents have no measured source,
  and Orca's answer for them is a hidden PTY that runs the agent's own `/usage`
  and scrapes the TUI, which is the part of their implementation that would be
  worst to copy.
