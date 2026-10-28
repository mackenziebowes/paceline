//! paceline: a launch gate for agents on a Claude Code subscription. `paceline statusline` runs as Claude Code's status
//! line and saves each rate-limit reading; `paceline gate` reads them and says LAUNCH or WAIT. This file is the surface:
//! it reads files, flags and the clock, hands them to `gate::decide`, and prints the answer.

mod gate;

use chrono::{Local, TimeZone};
use gate::{Agents, Burn, FiveHour, Gate, Sample, Snapshot, Targets, Verdict, WeekPace, Window};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::PathBuf;

const SAMPLE_EVERY_S: f64 = 300.0; // history keeps at most one unchanged sample per 5 minutes

const HELP: &str = "paceline: spend a Claude Code subscription's weekly limit on pace, and never hit the 5-hour cap

  paceline statusline            run as Claude Code's status line: saves each rate-limit reading, prints
                                 \"5h 12% · 7d 40% · <model>\"; never fails
  paceline gate [options]        LAUNCH (exit 0), WAIT until <time> (exit 1), or no fresh reading (exit 2)
      --week N                   percent of the weekly limit to spend by the reset (default 90)
      --max5h N                  never launch at or above this 5-hour percent (default 70)
      --first-day N              hours after the weekly reset that follow the 5-hour cap only (default 24)
      --stale N                  minutes before a reading is too old to trust (default 15)
      --agents N                 agents running now, for the \"week affords ~N agents\" line
      --agents-cmd CMD           a shell command that prints that number
      --json                     the decision as JSON
  paceline help

Files: $PACELINE_DIR, else $XDG_STATE_HOME/paceline, else ~/.local/state/paceline
       (reading.json, history.jsonl).";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("statusline") => statusline(),
        Some("gate") => gate_cmd(&args[1..]),
        Some("help" | "--help" | "-h") | None => {
            println!("{HELP}");
            0
        }
        Some(other) => {
            eprintln!("paceline: unknown command '{other}'. Run `paceline help` for the commands.");
            64
        }
    };
    std::process::exit(code);
}

fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("PACELINE_DIR") {
        return d.into();
    }
    if let Some(d) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(d).join("paceline");
    }
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state/paceline")
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

// ── statusline ──

fn statusline() -> i32 {
    let mut raw = String::new();
    let input: Value = std::io::stdin()
        .read_to_string(&mut raw)
        .ok()
        .and_then(|_| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null);
    let window = |k: &str| -> Option<Window> {
        let w = input.get("rate_limits")?.get(k)?;
        Some(Window { used_pct: w.get("used_percentage")?.as_f64()?, resets_at: w.get("resets_at")?.as_f64()? })
    };
    let snap = Snapshot { saved_at: now().floor(), week: window("seven_day"), five_hour: window("five_hour") };
    if snap.week.is_some() || snap.five_hour.is_some() {
        if let Err(e) = save(&snap) {
            eprintln!("paceline: couldn't save the reading in {}: {e}", dir().display());
        }
    }
    let mut parts = Vec::new();
    if let Some(w) = snap.five_hour {
        parts.push(format!("5h {:.0}%", w.used_pct));
    }
    if let Some(w) = snap.week {
        parts.push(format!("7d {:.0}%", w.used_pct));
    }
    if let Some(m) = input.get("model").and_then(|m| m.get("display_name")).and_then(Value::as_str) {
        parts.push(m.to_string());
    }
    println!("{}", parts.join(" · "));
    0
}

fn window_json(w: Option<Window>) -> Value {
    w.map(|w| json!({"used_pct": w.used_pct, "resets_at": w.resets_at})).unwrap_or(Value::Null)
}

/// The reading through a .tmp file and a rename, so a reader never sees half a file; then one history line, unless
/// the last one says the same and is under 5 minutes old.
fn save(s: &Snapshot) -> std::io::Result<()> {
    let d = dir();
    std::fs::create_dir_all(&d)?;
    let rec = json!({"saved_at": s.saved_at, "week": window_json(s.week), "five_hour": window_json(s.five_hour)});
    let tmp = d.join("reading.json.tmp");
    std::fs::write(&tmp, rec.to_string())?;
    std::fs::rename(&tmp, d.join("reading.json"))?;
    let Some(w) = s.week else { return Ok(()) };
    let line = json!({"t": s.saved_at, "week_pct": w.used_pct, "week_reset": w.resets_at,
                      "five_hour_pct": s.five_hour.map(|f| f.used_pct)});
    if let Some(last) = history_lines().last() {
        let same = last.get("week_pct") == line.get("week_pct") && last.get("five_hour_pct") == line.get("five_hour_pct");
        let recent = s.saved_at - last.get("t").and_then(Value::as_f64).unwrap_or(0.0) < SAMPLE_EVERY_S;
        if same && recent {
            return Ok(());
        }
    }
    let mut f = std::fs::OpenOptions::new().append(true).create(true).open(d.join("history.jsonl"))?;
    writeln!(f, "{line}")
}

fn history_lines() -> Vec<Value> {
    std::fs::read_to_string(dir().join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn load_snapshot() -> Option<Snapshot> {
    let v: Value = serde_json::from_slice(&std::fs::read(dir().join("reading.json")).ok()?).ok()?;
    let window = |k: &str| -> Option<Window> {
        let w = v.get(k)?;
        Some(Window { used_pct: w.get("used_pct")?.as_f64()?, resets_at: w.get("resets_at")?.as_f64()? })
    };
    Some(Snapshot { saved_at: v.get("saved_at")?.as_f64()?, week: window("week"), five_hour: window("five_hour") })
}

fn load_history() -> Vec<Sample> {
    history_lines()
        .iter()
        .filter_map(|r| {
            Some(Sample {
                t: r.get("t")?.as_f64()?,
                week_pct: r.get("week_pct")?.as_f64()?,
                week_reset: r.get("week_reset")?.as_f64()?,
            })
        })
        .collect()
}

// ── gate ──

struct GateArgs {
    targets: Targets,
    agents: AgentsSource,
    json: bool,
}

enum AgentsSource {
    None,
    Fixed(u32),
    Command(String),
}

fn parse_gate(args: &[String]) -> Result<GateArgs, String> {
    let mut t = Targets { week_pct: 90.0, five_hour_cap_pct: 70.0, first_day_h: 24.0, stale_s: 15.0 * 60.0 };
    let mut agents = AgentsSource::None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| -> Result<String, String> {
            it.next().cloned().ok_or_else(|| format!("{flag} needs a value after it. Run `paceline help` for an example."))
        };
        let num = |flag: &str, v: String| -> Result<f64, String> {
            v.trim().parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0)
                .ok_or_else(|| format!("{flag} needs a number of 0 or more, got '{v}'."))
        };
        match a.as_str() {
            "--week" => t.week_pct = num(a, value(a)?)?,
            "--max5h" => t.five_hour_cap_pct = num(a, value(a)?)?,
            "--first-day" => t.first_day_h = num(a, value(a)?)?,
            "--stale" => t.stale_s = num(a, value(a)?)? * 60.0,
            "--agents" => agents = AgentsSource::Fixed(num(a, value(a)?)? as u32),
            "--agents-cmd" => agents = AgentsSource::Command(value(a)?),
            "--json" => json = true,
            other => return Err(format!("unknown option '{other}'. Run `paceline help` for the options.")),
        }
    }
    if t.week_pct == 0.0 || t.week_pct > 100.0 {
        return Err(format!("--week must be above 0 and at most 100: it is the percent of the weekly limit to spend, got {}.", t.week_pct));
    }
    if t.five_hour_cap_pct > 100.0 {
        return Err(format!("--max5h must be at most 100: it is a percent of the 5-hour limit, got {}.", t.five_hour_cap_pct));
    }
    Ok(GateArgs { targets: t, agents, json })
}

fn running(src: &AgentsSource) -> Option<u32> {
    match src {
        AgentsSource::None => None,
        AgentsSource::Fixed(n) => Some(*n),
        AgentsSource::Command(cmd) => {
            let out = match std::process::Command::new("sh").arg("-c").arg(cmd).output() {
                Ok(out) => out,
                Err(e) => {
                    eprintln!("paceline gate: --agents-cmd could not run ({e}); the agents line is left out.");
                    return None;
                }
            };
            let text = String::from_utf8_lossy(&out.stdout);
            match text.trim().parse() {
                Ok(n) => Some(n),
                Err(_) => {
                    eprintln!("paceline gate: --agents-cmd {} printed '{}', not a whole number{}; the agents line is left out.", cmd, text.trim().chars().take(40).collect::<String>(), if out.status.success() { "" } else { " (it also exited non-zero)" });
                    None
                }
            }
        }
    }
}

fn gate_cmd(args: &[String]) -> i32 {
    let a = match parse_gate(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("paceline gate: {e}");
            return 64;
        }
    };
    let g = gate::decide(now(), load_snapshot(), &load_history(), running(&a.agents), a.targets);
    if a.json {
        println!("{}", serde_json::to_string_pretty(&g).unwrap_or_default());
    } else {
        print!("{}", text(&g, a.targets));
    }
    match g {
        Gate::Fresh { verdict: Verdict::Launch, .. } => 0,
        Gate::Fresh { verdict: Verdict::Wait { .. }, .. } => 1,
        Gate::Missing | Gate::Stale { .. } => 2,
    }
}

/// "Tue 11:00" in local time.
fn when(epoch: f64) -> String {
    Local.timestamp_opt(epoch.floor() as i64, 0).single().map(|t| t.format("%a %H:%M").to_string()).unwrap_or("?".into())
}

fn text(g: &Gate, t: Targets) -> String {
    let (verdict, week, five, burn) = match g {
        Gate::Missing => {
            return format!(
                "NO READING: nothing in {} yet. Set `paceline statusline` as Claude Code's status line and send one message.\n",
                dir().display()
            );
        }
        Gate::Stale { age_s } => {
            return format!(
                "STALE READING ({:.0} min old): send a message in any Claude Code session so the status line refreshes, then run this again.\n",
                age_s / 60.0
            );
        }
        Gate::Fresh { verdict, week, five_hour, burn } => (verdict, week, five_hour, burn),
    };
    let mut out = match verdict {
        Verdict::Launch => "LAUNCH\n".to_string(),
        Verdict::Wait { until } => format!("WAIT until {}\n", when(*until)),
    };
    let line = |used: f64, pace: f64, gone: f64, reset: f64| {
        format!(
            "week {used:.0}% used, pace line {pace:.1}% ({:.0}% of the week gone, target {:.0}%, resets {})\n",
            gone * 100.0, t.week_pct, when(reset)
        )
    };
    match *week {
        WeekPace::NoWeekWindow => out.push_str("week: no weekly window in the reading\n"),
        WeekPace::FirstDay { used_pct, pace_pct, hours_in, resets_at } => {
            out.push_str(&line(used_pct, pace_pct, hours_in / gate::WEEK_H, resets_at));
            out.push_str(&format!(
                "  first day after the reset ({} h in): following the 5-hour window, not the pace line\n",
                hours_in.floor()
            ));
        }
        WeekPace::OnPace { used_pct, pace_pct, week_gone, resets_at } => {
            out.push_str(&line(used_pct, pace_pct, week_gone, resets_at))
        }
        WeekPace::Ahead { used_pct, pace_pct, week_gone, resets_at, .. } => {
            out.push_str(&line(used_pct, pace_pct, week_gone, resets_at));
            out.push_str(&format!("  ahead of pace by {:.1} points\n", used_pct - pace_pct));
        }
    }
    match *five {
        FiveHour::NoFiveHourWindow => out.push_str("5h: no 5-hour window in the reading\n"),
        FiveHour::Under { used_pct, cap_pct, resets_at } | FiveHour::AtCap { used_pct, cap_pct, resets_at } => {
            out.push_str(&format!("5h {used_pct:.0}% (cap {cap_pct:.0}%, resets {})\n", when(resets_at)))
        }
    }
    match *burn {
        Burn::Calibrating { span_h, need_h } => out.push_str(&format!(
            "burn: calibrating ({:.0} min of samples, need {:.0})\n",
            span_h * 60.0,
            need_h * 60.0
        )),
        Burn::Measured { per_hour, span_h, sustainable_per_hour, agents } => {
            let n = match agents {
                Agents::Unknown => String::new(),
                Agents::Count { running, .. } => format!("; running agents {running}"),
            };
            out.push_str(&format!(
                "burn {per_hour:.2} %/h over {span_h:.1} h; sustainable {sustainable_per_hour:.2} %/h{n}\n"
            ));
            if let Agents::Count { running, affords: Some(a) } = agents {
                out.push_str(&format!(
                    "  per agent {:.3} %/h -> the week affords ~{a:.1} continuous agents\n",
                    per_hour / running as f64
                ));
            }
        }
    }
    out
}
