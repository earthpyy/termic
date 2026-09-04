#!/bin/bash
# Fixture "agent CLI" for e2e runs (see .claude/skills/e2e + docs/e2e-tests.md).
# Registered in the scratch profile so tasks spawn / resume / queue against a
# real PTY with ZERO tokens.
#
# Built to behave like `claude` so termic's agent-state UI (working indicator,
# attention badge, notifications) is exercised realistically:
#   - long-lived interactive PTY: stays alive until signalled, like a TUI.
#   - drives the OSC terminal title with claude's status glyphs — `✳` when
#     idle (work done), a Braille spinner while working. termic classifies
#     these exactly as it classifies real claude (see BUILTIN_TITLE_SIGNALS
#     `claude` in src/lib/agents.ts). The `fakeagent` registry entry must carry
#     the same `capabilities.signals` for the classifier to fire (the e2e
#     profile seeds them — keep the two in lock-step).
#   - one busy -> idle cycle per submitted line, mirroring "type a prompt, it
#     works, it goes idle".
#   - echoes its argv so a test can assert resume flags (--session-id/--resume,
#     --name) reached the spawn.

set -u

# OSC 0 window/icon title, ST-terminated (ESC \). Deliberately NOT BEL-
# terminated: a stray BEL would trip termic's bell -> attention heuristic.
set_title() { printf '\033]0;%s\033\\' "$1"; }

# Braille spinner frames — the "leading glyph that isn't ✳" claude uses while
# working, which termic's busy signal `^\s*[^A-Za-z0-9\s✳]` matches.
SPINNER=("⣷" "⣯" "⣟" "⡿" "⢿" "⣻" "⣽" "⣾")

# claude shows the task in its title; pull it from --name if the spawn passed one.
name="fakeagent"
prev=""
for a in "$@"; do
  [ "$prev" = "--name" ] && name="$a"
  prev="$a"
done

# On exit, drop back to the idle glyph and say goodbye (like a clean quit).
trap 'set_title "✳ ${name}"; printf "\nFAKE-AGENT exiting\n"; exit 0' INT TERM

# Cold start: banner + idle title (awaiting input == work done).
echo "FAKE-AGENT ready (args: $*)"
echo "  claude-like fixture: ✳ = idle, spinner = working. Type a prompt."
set_title "✳ ${name}"

# Signal drills for the work-done specs. Real claude reaches states that a
# plain echo loop never does, so a line starting with `#` is a directive rather
# than a prompt:
#
#   #pending N  reproduce the backgrounded-subagent trap: print claude's
#               "Waiting for N background agents to finish" status line and go
#               to the IDLE glyph while the work is still outstanding. Every
#               byte-stream signal then says "finished" and only that line says
#               otherwise, which is the whole point.
#   #settle     clear the pending line (the work landed) and go idle for real.
#   #stage      a multi-stage turn whose FIRST stage looks finished: idle glyph,
#               quiet PTY, still screen, so termic calls the turn done. Then the
#               agent goes back to work long after that done and finishes for
#               real. Both halves have to survive: the spinner has to come back
#               (a done we got wrong must not outlive the evidence), and the
#               real completion still has to fire (the turn's done token was
#               spent on the wrong one). The sleep is long enough to clear
#               STICKY_DONE_MS counted from when the done actually fires, not
#               from when the stage ends.
#   #hookstage  the same two-stage turn as #stage, but reported over the HOOK
#               transport (OSC 133;C/;D) rather than the title. A 133;D
#               reaches fireDone with `fromHook`, which bypasses the
#               one-done-per-submit token, so this is the path #stage cannot
#               cover. Real claude with shell integration emits this shape all
#               day (GH #276).
#   #osc9 TEXT  emit an OSC 9 notification with a verbatim body, the way claude
#               asks for the user. BEL-terminated, as claude sends it.
#   #usage BODY replay what claude's termic STATUS LINE writes: an OSC 777
#               carrying subscription usage (GH #277). BODY is everything after
#               the trusted `termic;` sender, e.g. `usage 58 41 - -`. Real
#               claude sends this on every turn from the script
#               agent_hooks::statusline_body generates; the wire format is
#               pinned in lib/agentUsage.ts. It must NEVER badge the tab, which
#               is the half a spec has to prove.
#   #bel        emit a REAL bell, distinct from the BEL that terminates an OSC.
#   #iip        emit an inline PNG, then Pi's alternate-screen redraw.
#   #hookattn   reproduce a claude PERMISSION PROMPT with termic's agent hook
#               installed. Real claude paints its IDLE glyph while it is
#               blocked on you (measured), which arms termic's 5s settle and
#               fires a false "done"; the hook's OSC 777 lands ~20ms later and
#               cancels it. This directive replays that exact order, so the
#               spec proves the attention wins the race rather than trusting
#               that it does. Body must match agentHooks.ts HOOK_OSC_BODY.
osc9()   { printf '\033]9;%s\007' "$1"; }
osc777() { printf '\033]777;notify;%s\007' "$1"; }
# The hook transport: what termic's installed scripts write.
osc133() { printf '\033]133;%s\007' "$1"; }
spin()   { for f in 0 1 2; do set_title "${SPINNER[$f]} ${name}"; sleep 0.15; done; }

# One "prompt" per stdin line: go busy (spinner title + streamed output), then
# return to the idle glyph — the busy -> idle transition claude drives, which
# termic turns into working -> done.
while IFS= read -r line; do
  # Strip leading interrupt bytes. A directive that reads a keystroke mid-turn
  # can be handed MORE than the one byte it consumes (xterm does not promise
  # one onData call per key), and the remainder then arrives glued to the front
  # of the next prompt: `#longwork` swallowed one Escape and left the tail to
  # turn the following `#longwork-silent` into an unrecognised line, which
  # dispatched to the default branch and silently tested nothing. Every
  # directive begins with `#`, so anything before it is debris.
  line="${line#"${line%%[!$'\x1b\x03']*}"}"
  case "$line" in
    "#pending "*)
      spin
      # Order matters: the status line must be the LAST thing painted, so it
      # sits at the bottom of the screen where the pending check looks.
      echo "FAKE-AGENT backgrounded ${line#\#pending } agent(s)"
      echo "✻ Waiting for ${line#\#pending } background agents to finish"
      set_title "✳ ${name}"              # idle glyph WHILE work is outstanding
      continue ;;
    "#settle")
      # Enough lines to push the pending status line out of the bottom-of-screen
      # window the check looks at. That IS the real behaviour: claude's words
      # stay in the scrollback, they just stop being the live status.
      echo "FAKE-AGENT all background work landed"
      for i in 1 2 3 4 5 6 7 8 9 10; do echo "FAKE-AGENT result line ${i}"; done
      set_title "✳ ${name}"
      continue ;;
    "#stage")
      # ~1.5s of visible work before the misleading idle glyph — just enough for
      # termic to latch "working" (observed: ~0.7s from submit to badge).
      # This used to be ~6s: the done that follows only badges on a tab nobody
      # is watching, and the spec backgrounded the task by CREATING the second
      # one here (~1.5s), which raced the spinner. The spec now creates that
      # task up front and backgrounds with a store call, so the padding is gone.
      for i in $(seq 1 5); do set_title "${SPINNER[$((i % 8))]} ${name}"; sleep 0.3; done
      echo "FAKE-AGENT stage 1 landed"
      set_title "✳ ${name}"              # looks finished, isn't
      sleep 16
      spin                               # stage 2: back to work
      echo "FAKE-AGENT stage 2 landed"
      sleep 2
      set_title "✳ ${name}"              # finished for real this time
      continue ;;
    "#hookstage")
      # The SAME two-stage turn as #stage, but reported over the HOOK
      # transport (OSC 133;C/;D) instead of the title. It exists because the
      # two paths reach `fireDone` with opposite guards and only one of them
      # was ever covered: a 133;D calls it with `fromHook`, which bypasses the
      # one-done-per-submit token outright, so nothing at all stood between a
      # multi-command turn and one notification per command. Real claude with
      # shell integration emits exactly this shape - a captured
      # `termic-workstate.log` shows `CDCDCDCDCD` on ordinary tasks (GH #276).
      #
      # No title is painted here, deliberately. Mixing the two would let the
      # title path account for a transition the hook path was supposed to
      # prove, which is how the hook half stayed untested in the first place.
      osc133 "C"
      echo "FAKE-AGENT hook stage 1 working"
      sleep 1
      osc133 "D"                         # turn "ends" - badge #1
      echo "FAKE-AGENT hook stage 1 landed"
      sleep 16                           # clear STICKY_DONE_MS from the done
      osc133 "C"                         # back to work, which clears the badge
      echo "FAKE-AGENT hook stage 2 working"
      sleep 1
      osc133 "D"                         # ends for real - badge #2 today
      echo "FAKE-AGENT hook stage 2 landed"
      continue ;;
    "#osc9 "*)
      osc9 "${line#\#osc9 }"
      continue ;;
    "#usage "*)
      # Sender field is `termic`, exactly as the generated status line writes
      # it: the body is only trusted when it is.
      osc777 "termic;${line#\#usage }"
      continue ;;
    "#bel")
      printf '\007'
      continue ;;
    "#iip")
      # Match Pi's IIP redraw: clear the screen, reserve image rows, emit the
      # image from the last reserved row, then repeat after a layout shift.
      rows=$(stty size <&0 2>/dev/null | cut -d' ' -f1)
      rows=${rows:-24}
      printf '\033[?1049h'
      for top in 1 2; do
        printf '\033[?2026h\033[2J'
        image_row=$((top + 20))
        for ((row = 1; row <= rows; row++)); do
          printf '\033[%s;1H\033[2K' "$row"
          if ((row == image_row)); then
            printf '\033[20A'
            cat "$(dirname "$0")/../e2e/fixtures/iip/termic-icon.iip"
          elif ((row == image_row + 1)); then
            printf 'Pi redraw %s' "$top"
          fi
        done
        printf '\033[?2026l'
        sleep 0.1
      done
      set_title "✳ ${name} iip-after"
      continue ;;
    "#longwork")
      # A turn long enough for a spec to interrupt it. The first attempt at
      # this raced a ~1s turn and pressed the key after it had already ended,
      # which is the same mistake that invalidated the first live interrupt
      # probe: an interrupt test has to interrupt something.
      #
      # It also HONOURS the interrupt, because that is what a real agent does:
      # claude stops and repaints its idle glyph ~90ms after Escape. A fixture
      # that kept spinning would be testing an agent that ignored the user.
      # `read -t 1` doubles as the frame delay and the input check; integer
      # timeouts only, since macOS ships bash 3.2 where fractional ones fail.
      # Starts the turn the way a hooked agent does. Hooks own both edges now,
      # so a busy title alone cannot set working for an agent that reports its
      # own state.
      osc133 C
      for i in $(seq 1 60); do
        set_title "${SPINNER[$((i % 8))]} ${name}"
        if IFS= read -r -t 1 -N 1 _key; then
          echo "FAKE-AGENT interrupted"
          set_title "✳ ${name}"
          continue 2
        fi
      done
      set_title "✳ ${name}"
      continue ;;
    "#longwork-silent")
      # The agy shape: a long turn that HONOURS the interrupt but reports it
      # through neither a hook nor a title, so the terminal simply falls quiet.
      # Measured: agy fires nothing at all on Escape or Ctrl-C and has no title
      # state to read, which leaves the terminal going quiet as the only
      # evidence the user's key landed. It is the only agent in that position:
      # claude repaints its idle glyph, grok has StopCancelled, and opencode
      # reports session.idle on the second Escape.
      #
      # The busy title is painted ONCE and never cleared, including on the
      # interrupt. That is what isolates the path under test: termic's other
      # interrupt route needs the title to go idle, so if the badge clears here
      # it can only have been the terminal falling quiet. It also makes a
      # mis-dispatch loud, since the default branch below ends on the idle
      # glyph and the spec asserts the busy one is still there.
      set_title "${SPINNER[1]} ${name}"
      # The turn STARTS the way a hooked agent starts one. Since hooks own both
      # edges, a busy title no longer sets working for an agent that reports
      # its own state, so a fixture without this could never reach working and
      # would be testing an agent that does not exist.
      osc133 C
      for i in $(seq 1 60); do
        printf '.'
        if IFS= read -r -t 1 -N 1 _key; then
          echo "FAKE-AGENT interrupted"
          continue 2
        fi
      done
      continue ;;
    "#hookturn")
      # A hooked agent mid-turn whose TITLE then goes idle while the turn is
      # still outstanding: 133;C and no 133;D. Exactly claude's shape when it
      # backgrounds subagents, and the case the whole design turns on, so the
      # fixture has to produce it rather than the spec faking a state.
      osc133 C
      spin
      echo "FAKE-AGENT echo: ${line}"
      set_title "✳ ${name}"
      continue ;;
    "#hookattn")
      spin
      echo "FAKE-AGENT needs your permission to continue"
      set_title "✳ ${name}"                       # the lie: idle while blocked
      osc777 "termic;agent needs your input"      # the hook, right behind it
      continue ;;
  esac
  spin
  echo "FAKE-AGENT echo: ${line}"        # streamed "response"
  set_title "✳ ${name}"                  # done: idle glyph
done
