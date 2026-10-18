//! The gate's decision: a pure function from a reading, the history and the targets to a verdict. Nothing here reads a
//! file or a clock; `main.rs` hands everything in. Each state is one variant with only its own fields.

use serde::Serialize;

pub const WEEK_H: f64 = 168.0;
pub const BURN_WINDOW_H: f64 = 3.0; // burn is measured over the last 3 hours of samples
pub const BURN_MIN_SPAN_H: f64 = 1.0; // ...and needs an hour of them (the weekly % moves in whole points)

/// One rate-limit window as Claude Code's status line reports it.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Window {
    pub used_pct: f64,
    /// Epoch seconds.
    pub resets_at: f64,
}

/// What the status line last saw. Either window can be absent (an API-key session reports none).
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Snapshot {
    pub saved_at: f64,
    pub week: Option<Window>,
    pub five_hour: Option<Window>,
}

/// One history line: the weekly %, tagged with the weekly reset it belongs to.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub t: f64,
    pub week_pct: f64,
    pub week_reset: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Targets {
    /// Percent of the weekly limit to have spent by the reset.
    pub week_pct: f64,
    /// Never launch while the 5-hour window is at or above this percent.
    pub five_hour_cap_pct: f64,
    /// For this many hours after the weekly reset, the pace line is set aside.
    pub first_day_h: f64,
    /// A reading older than this is not trusted.
    pub stale_s: f64,
}

/// Where the week stands against its pace line.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WeekPace {
    NoWeekWindow,
    /// The line starts near zero after a reset, so it would say WAIT at a few percent; follow the 5-hour window.
    FirstDay { used_pct: f64, pace_pct: f64, hours_in: f64, resets_at: f64 },
    OnPace { used_pct: f64, pace_pct: f64, week_gone: f64, resets_at: f64 },
    Ahead { used_pct: f64, pace_pct: f64, week_gone: f64, resets_at: f64, wait_until: f64 },
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FiveHour {
    NoFiveHourWindow,
    Under { used_pct: f64, cap_pct: f64, resets_at: f64 },
    AtCap { used_pct: f64, cap_pct: f64, resets_at: f64 },
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Burn {
    Calibrating { span_h: f64, need_h: f64 },
    Measured {
        /// Weekly percentage points per hour, over the last few hours.
        per_hour: f64,
        span_h: f64,
        /// Headroom left this week spread over the hours left.
        sustainable_per_hour: f64,
        agents: Agents,
    },
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Agents {
    Unknown,
    Count { running: u32, affords: Option<f64> },
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    Launch,
    Wait { until: f64 },
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "reading", rename_all = "snake_case")]
pub enum Gate {
    Missing,
    Stale { age_s: f64 },
    Fresh { verdict: Verdict, week: WeekPace, five_hour: FiveHour, burn: Burn },
}

pub fn decide(now: f64, snapshot: Option<Snapshot>, history: &[Sample], running: Option<u32>, t: Targets) -> Gate {
    let Some(s) = snapshot else { return Gate::Missing };
    let age = now - s.saved_at;
    if age > t.stale_s {
        return Gate::Stale { age_s: age };
    }
    let week = week_pace(now, s.week, t);
    let five_hour = match s.five_hour {
        None => FiveHour::NoFiveHourWindow,
        Some(w) if w.used_pct >= t.five_hour_cap_pct => {
            FiveHour::AtCap { used_pct: w.used_pct, cap_pct: t.five_hour_cap_pct, resets_at: w.resets_at }
        }
        Some(w) => FiveHour::Under { used_pct: w.used_pct, cap_pct: t.five_hour_cap_pct, resets_at: w.resets_at },
    };
    let wait = match (week, five_hour) {
        (WeekPace::Ahead { wait_until, .. }, FiveHour::AtCap { resets_at, .. }) => Some(wait_until.max(resets_at)),
        (WeekPace::Ahead { wait_until, .. }, _) => Some(wait_until),
        (_, FiveHour::AtCap { resets_at, .. }) => Some(resets_at),
        _ => None,
    };
    let verdict = match wait {
        Some(until) => Verdict::Wait { until },
        None => Verdict::Launch,
    };
    Gate::Fresh { verdict, week, five_hour, burn: burn(now, s.week, history, running, t) }
}

fn week_pace(now: f64, week: Option<Window>, t: Targets) -> WeekPace {
    let Some(w) = week else { return WeekPace::NoWeekWindow };
    let left_h = (w.resets_at - now) / 3600.0;
    let week_gone = (1.0 - left_h / WEEK_H).clamp(0.0, 1.0);
    let pace = t.week_pct * week_gone;
    let hours_in = WEEK_H - left_h;
    if (0.0..t.first_day_h).contains(&hours_in) {
        WeekPace::FirstDay { used_pct: w.used_pct, pace_pct: pace, hours_in, resets_at: w.resets_at }
    } else if w.used_pct > pace {
        // Waiting until the line catches up with what is already spent.
        let wait_until = now + (w.used_pct - pace) / t.week_pct * WEEK_H * 3600.0;
        WeekPace::Ahead { used_pct: w.used_pct, pace_pct: pace, week_gone, resets_at: w.resets_at, wait_until }
    } else {
        WeekPace::OnPace { used_pct: w.used_pct, pace_pct: pace, week_gone, resets_at: w.resets_at }
    }
}

fn burn(now: f64, week: Option<Window>, history: &[Sample], running: Option<u32>, t: Targets) -> Burn {
    let rows: Vec<&Sample> = history
        .iter()
        .filter(|s| week.is_some_and(|w| s.week_reset == w.resets_at))
        .filter(|s| s.t <= now && now - s.t <= BURN_WINDOW_H * 3600.0)
        .collect();
    let span_h = match (rows.first(), rows.last()) {
        (Some(a), Some(b)) => (b.t - a.t) / 3600.0,
        _ => 0.0,
    };
    let (Some(first), Some(last), Some(w)) = (rows.first(), rows.last(), week) else {
        return Burn::Calibrating { span_h, need_h: BURN_MIN_SPAN_H };
    };
    if span_h < BURN_MIN_SPAN_H {
        return Burn::Calibrating { span_h, need_h: BURN_MIN_SPAN_H };
    }
    let per_hour = (last.week_pct - first.week_pct) / span_h;
    let hours_left = ((w.resets_at - now) / 3600.0).max(0.25);
    let sustainable = (t.week_pct - w.used_pct).max(0.0) / hours_left;
    let agents = match running {
        None => Agents::Unknown,
        Some(n) => Agents::Count {
            running: n,
            affords: (n > 0 && per_hour > 0.0).then(|| sustainable / (per_hour / n as f64)),
        },
    };
    Burn::Measured { per_hour, span_h, sustainable_per_hour: sustainable, agents }
}
