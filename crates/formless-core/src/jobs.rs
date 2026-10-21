//! Scheduled jobs (SPEC §9.7): a prompt that runs later (`at`) or
//! periodically (`cron`, local time).

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike};

/// A parsed 5-field cron expression: minute hour day-of-month month
/// day-of-week (0 or 7 = Sunday). Supports `*`, numbers, `a-b`, lists
/// and `/step`, plus `@hourly`, `@daily`, `@weekly`, `@monthly`.
#[derive(Debug, Clone, PartialEq)]
pub struct Cron {
    minute: Vec<bool>,
    hour: Vec<bool>,
    dom: Vec<bool>,
    month: Vec<bool>,
    dow: Vec<bool>,
    dom_any: bool,
    dow_any: bool,
}

fn field(spec: &str, min: u32, max: u32, what: &str) -> Result<Vec<bool>> {
    let mut set = vec![false; max as usize + 1];
    for part in spec.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (
                r,
                s.parse::<u32>()
                    .ok()
                    .filter(|s| *s > 0)
                    .with_context(|| format!("bad step in {what}: {part}"))?,
            ),
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (a.parse()?, b.parse()?)
        } else {
            let v: u32 = range
                .parse()
                .with_context(|| format!("bad {what}: {part}"))?;
            (v, if part.contains('/') { max } else { v })
        };
        if lo < min || hi > max || lo > hi {
            bail!("{what} out of range ({min}-{max}): {part}");
        }
        for v in (lo..=hi).step_by(step as usize) {
            set[v as usize] = true;
        }
    }
    Ok(set)
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Self> {
        let expr = match expr.trim() {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            e => e,
        };
        let f: Vec<&str> = expr.split_whitespace().collect();
        let [mi, h, dom, mo, dow] = f.as_slice() else {
            bail!("cron needs 5 fields (minute hour day month weekday): {expr}");
        };
        let mut dow_set = field(dow, 0, 7, "weekday")?;
        if dow_set[7] {
            dow_set[0] = true;
        }
        dow_set.truncate(7);
        Ok(Self {
            minute: field(mi, 0, 59, "minute")?,
            hour: field(h, 0, 23, "hour")?,
            dom: field(dom, 1, 31, "day")?,
            month: field(mo, 1, 12, "month")?,
            dow: dow_set,
            dom_any: *dom == "*",
            dow_any: *dow == "*",
        })
    }

    fn matches(&self, t: &DateTime<Local>) -> bool {
        let day = self.dom[t.day() as usize];
        let wd = self.dow[t.weekday().num_days_from_sunday() as usize];
        // Cron rule: with both day fields restricted, either may match.
        let day_ok = match (self.dom_any, self.dow_any) {
            (false, false) => day || wd,
            _ => day && wd,
        };
        self.minute[t.minute() as usize]
            && self.hour[t.hour() as usize]
            && self.month[t.month() as usize]
            && day_ok
    }

    /// The first matching minute strictly after `after` (within ~4 years).
    pub fn next_after(&self, after: DateTime<Local>) -> Option<DateTime<Local>> {
        let start = after.with_second(0)?.with_nanosecond(0)? + Duration::minutes(1);
        let mut t = start;
        for _ in 0..(4 * 366 * 24 * 60) {
            if self.matches(&t) {
                return Some(t);
            }
            // Skip whole hours/days that cannot match, minute by minute otherwise.
            t = if !self.hour[t.hour() as usize] {
                (t + Duration::hours(1)).with_minute(0)?
            } else {
                t + Duration::minutes(1)
            };
        }
        None
    }
}

/// When a one-shot job runs: RFC 3339, or relative "in 30m" / "2h" / "1d".
pub fn parse_at(at: &str, now: DateTime<Local>) -> Result<DateTime<Local>> {
    let s = at.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Local));
    }
    if let Ok(t) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M") {
        return Local
            .from_local_datetime(&t)
            .single()
            .context("ambiguous local time");
    }
    let rel = s.strip_prefix("in ").unwrap_or(s).trim();
    let (num, unit) = rel.split_at(rel.find(|c: char| !c.is_ascii_digit()).unwrap_or(rel.len()));
    let n: i64 = num.parse().map_err(|_| {
        anyhow!("at must be RFC 3339, \"YYYY-MM-DD HH:MM\" or like \"in 30m\": {at}")
    })?;
    let d = match unit.trim() {
        "m" | "min" | "mins" | "minute" | "minutes" => Duration::minutes(n),
        "h" | "hour" | "hours" => Duration::hours(n),
        "d" | "day" | "days" => Duration::days(n),
        u => bail!("unknown time unit '{u}' in at: {at}"),
    };
    Ok(now + d)
}

/// A job's first run: the next cron match, or the `at` time.
pub fn first_run(
    cron: Option<&str>,
    at: Option<&str>,
    now: DateTime<Local>,
) -> Result<DateTime<Local>> {
    match (cron, at) {
        (Some(c), _) => Cron::parse(c)?
            .next_after(now)
            .context("cron never matches"),
        (None, Some(a)) => parse_at(a, now),
        (None, None) => bail!("give cron (recurring) or at (one-shot)"),
    }
}
