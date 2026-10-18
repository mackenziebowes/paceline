# paceline

A launch gate for AI agents on a Claude Code subscription. Before a lead agent starts another agent, it runs `paceline gate` and gets **LAUNCH** or **WAIT until Tue 11:00**.

The gate checks two limits:

- **The weekly limit, against a pace line.** The line rises evenly from 0 at the weekly reset to your target (90% by default) at the next reset. If you've spent more than the line, the gate says WAIT until the line catches up. If you've spent less, the subscription is going unused, and the gate says LAUNCH.
- **The 5-hour window, against a safety cap** (70% by default). An agent that hits the 5-hour limit stops mid-task, so the gate stops launching first.

For the first 24 hours after a weekly reset, the gate sets the pace line aside and follows only the 5-hour cap. The line starts near zero, so without this it would say WAIT after a few percent, and agents would lose their warm caches waiting.

## Install

```bash
cargo install --git https://github.com/mackenziebowes/paceline
```

## Set up

Claude Code passes its rate-limit reading to the status line. Make paceline your status line in `~/.claude/settings.json`:

```json
{ "statusLine": { "type": "command", "command": "paceline statusline" } }
```

It saves each reading and prints `5h 12% · 7d 40% · Opus 5.5`. If you already have a status line, call `paceline statusline > /dev/null` from your script, with the same JSON on stdin.

## Use

An example after a few hours of readings, with three agents running:

```text
$ paceline gate --agents 3
LAUNCH
week 58% used, pace line 62.3% (69% of the week gone, target 90%, resets Tue 11:00)
5h 1% (cap 70%, resets Sun 11:40)
burn 1.20 %/h over 2.5 h; sustainable 0.85 %/h; running agents 3
  per agent 0.400 %/h -> the week affords ~2.1 continuous agents
```

Exit codes: `0` LAUNCH, `1` WAIT, `2` no fresh reading. A reading is fresh for 15 minutes; send any message in a session to refresh it.

| Option | Default | |
|---|---|---|
| `--week N` | 90 | percent of the weekly limit to spend by the reset |
| `--max5h N` | 70 | never launch at or above this 5-hour percent |
| `--first-day N` | 24 | hours after the reset that follow the 5-hour cap only |
| `--stale N` | 15 | minutes before a reading is too old |
| `--agents N` / `--agents-cmd CMD` | | how many agents run now, for the "affords" line |
| `--json` | | the decision as JSON |

The burn line needs an hour of readings from the last three hours. Until then it says `calibrating`.

Readings go in `$PACELINE_DIR`, else `$XDG_STATE_HOME/paceline`, else `~/.local/state/paceline`.

## In an agent's instructions

> Before you launch an agent, run `paceline gate`. On WAIT, don't launch: do the work yourself or wait until the time it prints.

## Licence

MIT
