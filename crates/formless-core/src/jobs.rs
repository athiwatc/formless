//! Scheduled jobs (SPEC §9.7): a prompt that runs later (`at`) or
//! periodically (`cron`, local time).

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike};

/// A parsed 5-field cron expression: minute hour day-of-month month
/// day-of-week (0 or 7 = Sunday). Supports `*`, numbers, `a-b`, lists
/// and `/step`, three-letter month/weekday names, plus `@hourly`,
/// `@daily`, `@weekly`, `@monthly`, `@yearly` and `@annually`.
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
    let names: &[&str] = match what {
        "month" => &[
            "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
        ],
        "weekday" => &["sun", "mon", "tue", "wed", "thu", "fri", "sat"],
        _ => &[],
    };
    let mut set = vec![false; max as usize + 1];
    for part in spec.split(',') {
        let value = |token: &str| -> Result<u32> {
            names
                .iter()
                .position(|name| token.eq_ignore_ascii_case(name))
                .map(|i| min + i as u32)
                .or_else(|| token.parse::<u32>().ok())
                .with_context(|| format!("bad {what}: {part}"))
        };
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
            (value(a)?, value(b)?)
        } else {
            let v = value(range)?;
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
            "@yearly" | "@annually" => "0 0 1 1 *",
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
            // Cronie flags depend on leading syntax, not the generated value set.
            dom_any: dom.starts_with('*'),
            dow_any: dow.starts_with('*'),
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
        let mut t = next_local_minute(&after)?;
        for _ in 0..(4 * 366 * 24 * 60) {
            if self.matches(&t) {
                return Some(t);
            }
            t = next_local_minute(&t)?;
        }
        None
    }
}

// Work from absolute seconds. Rebuilding a wall-clock value would discard
// both occurrences of a repeated local minute as ambiguous.
fn next_local_minute(after: &DateTime<Local>) -> Option<DateTime<Local>> {
    let seconds = i64::from(60 - after.second());
    let candidate = Local
        .timestamp_opt(after.timestamp().checked_add(seconds)?, 0)
        .single()?;
    if candidate.second() == 0 {
        return Some(candidate);
    }
    // A historical offset change can include seconds. Look for the next
    // local minute boundary without skipping over it on the new offset.
    let mut current = Local
        .timestamp_opt(after.timestamp().checked_add(1)?, 0)
        .single()?;
    while current.second() != 0 {
        current = current.checked_add_signed(Duration::seconds(1))?;
    }
    Some(current)
}

/// When a one-shot job runs: strictly future RFC 3339/local time, or a
/// checked relative offset of at most 366 days.
pub fn parse_at(at: &str, now: DateTime<Local>) -> Result<DateTime<Local>> {
    let s = at.trim();
    let target = if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        t.with_timezone(&Local)
    } else if let Ok(t) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M") {
        Local
            .from_local_datetime(&t)
            .single()
            .context("ambiguous or nonexistent local time")?
    } else {
        let rel = s.strip_prefix("in ").unwrap_or(s).trim();
        let (num, unit) =
            rel.split_at(rel.find(|c: char| !c.is_ascii_digit()).unwrap_or(rel.len()));
        let n: i64 = num.parse().map_err(|_| {
            anyhow!("at must be RFC 3339, \"YYYY-MM-DD HH:MM\" or like \"in 30m\": {at}")
        })?;
        let seconds_per_unit = match unit.trim() {
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "h" | "hour" | "hours" => 60 * 60,
            "d" | "day" | "days" => 24 * 60 * 60,
            u => bail!("unknown time unit '{u}' in at: {at}"),
        };
        let seconds = n
            .checked_mul(seconds_per_unit)
            .filter(|seconds| *seconds <= 366 * 24 * 60 * 60)
            .context("relative job time must be at most 366 days")?;
        let delta = Duration::try_seconds(seconds).context("relative job time is out of range")?;
        now.checked_add_signed(delta)
            .context("job time is out of range")?
    };
    if target <= now {
        bail!("job time must be strictly in the future; use jobs run ID for an immediate firing");
    }
    Ok(target)
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

/// A schedule as the user wrote it: a cron expression (or `@daily`, …),
/// or plain English such as "every weekday at 9am", "every 2 hours",
/// "daily at 18:30", "every monday and friday at noon" (SPEC §9.7).
/// Returns the cron expression to store.
pub fn to_cron(schedule: &str) -> Result<String> {
    let s = schedule.trim();
    let cron_error = match Cron::parse(s) {
        Ok(_) => return Ok(s.to_string()),
        Err(error) => error,
    };
    // Natural phrases can also contain five whitespace-separated words,
    // e.g. "every weekday at 9 a.m.". Try them before reporting cron errors.
    if let Some(cron) = natural_to_cron(s) {
        return Ok(cron);
    }
    if s.starts_with('@') || s.split_whitespace().count() == 5 {
        bail!("invalid cron expression: {cron_error}");
    }
    bail!(
        "not a cron expression or a schedule like \"every weekday at 9am\", \"every 2 hours\", \"daily at 18:30\": {schedule}"
    )
}

/// The next `n` run times of a cron expression after `now`.
pub fn next_runs(cron: &str, now: DateTime<Local>, n: usize) -> Result<Vec<DateTime<Local>>> {
    let c = Cron::parse(cron)?;
    let mut out = Vec::new();
    let mut t = now;
    while out.len() < n {
        match c.next_after(t) {
            Some(next) => {
                out.push(next);
                t = next;
            }
            None => break,
        }
    }
    Ok(out)
}

const DAYS: [(&str, u32); 7] = [
    ("sunday", 0),
    ("monday", 1),
    ("tuesday", 2),
    ("wednesday", 3),
    ("thursday", 4),
    ("friday", 5),
    ("saturday", 6),
];
const MORNING: (u32, u32) = (9, 0);

/// "9am", "9:30 p.m.", "18:30", "morning", "noon", "midnight" → (hour, minute).
fn parse_time(t: &str) -> Option<(u32, u32)> {
    let t = t.trim().replace(' ', "").to_ascii_lowercase();
    match t.as_str() {
        "morning" => return Some(MORNING),
        "noon" | "midday" => return Some((12, 0)),
        "midnight" => return Some((0, 0)),
        _ => {}
    }
    let (body, pm) = if let Some(b) = ["p.m.", "p.m", "pm.", "pm"]
        .into_iter()
        .find_map(|suffix| t.strip_suffix(suffix))
    {
        (b, Some(true))
    } else if let Some(b) = ["a.m.", "a.m", "am.", "am"]
        .into_iter()
        .find_map(|suffix| t.strip_suffix(suffix))
    {
        (b, Some(false))
    } else {
        (t.as_str(), None)
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (body.parse::<u32>().ok()?, 0),
    };
    let h = match pm {
        Some(true) if h < 12 => h + 12,
        Some(false) if h == 12 => 0,
        Some(_) if h > 12 => return None,
        _ => h,
    };
    (h < 24 && m < 60).then_some((h, m))
}

fn natural_to_cron(s: &str) -> Option<String> {
    let lower = s.to_lowercase();
    let lower = lower.trim().trim_end_matches('.');
    // "… at TIME" (default midnight for day-based schedules).
    let (head, time) = match lower.rsplit_once(" at ") {
        Some((h, t)) => (h.trim(), Some(parse_time(t)?)),
        None => (lower, None),
    };
    let head = head.strip_prefix("every ").unwrap_or(head).trim();
    let morning_days = head.strip_suffix(" morning").filter(|_| time.is_none());
    let head = morning_days.map(str::trim_end).unwrap_or(head);
    let default = if head == "morning" || morning_days.is_some() {
        MORNING
    } else {
        (0, 0)
    };
    let (h, m) = time.unwrap_or(default);
    // Interval schedules: "every 15 minutes", "every 2 hours", "hourly".
    let words: Vec<&str> = head.split_whitespace().collect();
    let interval = |n: u32, unit: &str| -> Option<String> {
        match unit.trim_end_matches('s') {
            "minute" | "min" if (1..60).contains(&n) && time.is_none() => Some(if n == 1 {
                "* * * * *".into()
            } else {
                format!("*/{n} * * * *")
            }),
            "hour" if (1..24).contains(&n) && time.is_none() => Some(if n == 1 {
                "0 * * * *".into()
            } else {
                format!("0 */{n} * * *")
            }),
            "day" if n == 1 => Some(format!("{m} {h} * * *")),
            _ => None,
        }
    };
    if morning_days.is_none() {
        match words.as_slice() {
            ["hourly"] | ["hour"] => return interval(1, "hour"),
            ["minute"] => return interval(1, "minute"),
            ["daily"] | ["day"] | ["night"] | ["morning"] => {
                return Some(format!("{m} {h} * * *"));
            }
            [n, unit] if n.parse::<u32>().is_ok() => return interval(n.parse().ok()?, unit),
            ["week"] | ["weekly"] => return Some(format!("{m} {h} * * 1")),
            ["month"] | ["monthly"] => return Some(format!("{m} {h} 1 * *")),
            ["year"] | ["yearly"] | ["annually"] => return Some(format!("{m} {h} 1 1 *")),
            _ => {}
        }
    }
    match words.as_slice() {
        ["weekday"] | ["weekdays"] => return Some(format!("{m} {h} * * 1-5")),
        ["weekend"] | ["weekends"] => return Some(format!("{m} {h} * * 0,6")),
        _ => {}
    }
    // Named days: "monday", "monday and friday", "mon, wed, fri".
    let mut days = Vec::new();
    for w in head
        .replace(" and ", ",")
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|w| !w.is_empty() && *w != "on")
    {
        let w = w.trim_end_matches('s');
        let (_, d) = DAYS
            .iter()
            .find(|(name, _)| w.len() >= 3 && name.starts_with(w))?;
        if !days.contains(d) {
            days.push(*d);
        }
    }
    if days.is_empty() {
        return None;
    }
    days.sort_unstable();
    let list: Vec<String> = days.iter().map(u32::to_string).collect();
    Some(format!("{m} {h} * * {}", list.join(",")))
}

/// How many occurrences of `cron` fell after `due` and up to `now`
/// besides the one being run now (capped): firings missed while the
/// daemon was down or the machine slept.
pub fn missed_since(cron: &str, due: DateTime<Local>, now: DateTime<Local>) -> usize {
    let Ok(c) = Cron::parse(cron) else {
        return 0;
    };
    let mut n = 0;
    let mut t = due;
    while let Some(next) = c.next_after(t) {
        if next > now || n >= 10_000 {
            break;
        }
        n += 1;
        t = next;
    }
    n
}

/// Deliver a finished firing's answer (SPEC §9.7): a desktop
/// notification, an appended file, and/or a webhook POST. Returns one
/// line per target saying how it went.
pub async fn deliver(
    target: &serde_json::Value,
    http: &reqwest::Client,
    job: &formless_protocol::JobInfo,
    status: &str,
    answer: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    let title = format!("Job {}: {status}", job.id);
    if target["notify"] == true {
        let body: String = answer.chars().take(200).collect();
        out.push(match notify(&title, &body) {
            Ok(()) => "notified".into(),
            Err(e) => format!("notify failed: {e}"),
        });
    }
    if let Some(path) = target["file"].as_str() {
        let entry = format!(
            "\n## {} — {} ({status})\n\n{}\n",
            Local::now().format("%Y-%m-%d %H:%M"),
            job.prompt,
            answer.trim()
        );
        let wrote = (|| -> std::io::Result<()> {
            use std::io::Write;
            if let Some(dir) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?
                .write_all(entry.as_bytes())
        })();
        out.push(match wrote {
            Ok(()) => format!("appended to {path}"),
            Err(e) => format!("writing {path} failed: {e}"),
        });
    }
    if let Some(url) = target["webhook"].as_str() {
        let body = serde_json::json!({
            "job_id": job.id, "prompt": job.prompt, "status": status,
            "answer": answer, "session_id": job.session_id,
        });
        let sent = http
            .post(url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await;
        out.push(match sent {
            Ok(r) if r.status().is_success() => format!("posted to {url}"),
            Ok(r) => format!("webhook {url} answered {}", r.status()),
            Err(e) => format!("webhook {url} failed: {e}"),
        });
    }
    out
}

fn notify(title: &str, body: &str) -> std::result::Result<(), String> {
    #[cfg(target_os = "macos")]
    let (cmd, args) = {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        (
            "osascript",
            vec![
                "-e".to_string(),
                format!(
                    "display notification \"{}\" with title \"{}\"",
                    esc(body),
                    esc(title)
                ),
            ],
        )
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let (cmd, args) = ("notify-send", vec![title.to_string(), body.to_string()]);
    #[cfg(not(unix))]
    {
        let _ = (title, body);
        return Err("desktop notifications are not supported here".into());
    }
    #[cfg(unix)]
    {
        let status = std::process::Command::new(cmd)
            .args(&args)
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("{cmd} exited with {status}"))
        }
    }
}

#[cfg(test)]
mod schedule_tests {
    use super::*;

    #[test]
    fn leading_wildcard_steps_constrain_day_field_matches() {
        let monday = Local.with_ymd_and_hms(2026, 1, 5, 9, 0, 0).unwrap();
        let tuesday = Local.with_ymd_and_hms(2026, 1, 6, 9, 0, 0).unwrap();
        let cron = Cron::parse("0 9 */1 * 1").unwrap();
        assert!(cron.matches(&monday));
        assert!(
            !cron.matches(&tuesday),
            "wildcard-step DOM must still require Monday"
        );
    }

    #[test]
    fn day_field_flags_follow_leading_syntax_not_value_coverage() {
        for (expr, day, expected) in [
            ("0 9 */2 * 1", 5, true),
            ("0 9 */2 * 1", 12, false),
            ("0 9 */2 * 1", 19, true),
            ("0 9 15 * */1", 5, false),
            ("0 9 15 * */1", 15, true),
            ("0 9 15 * 1", 5, true),
            ("0 9 15 * 1", 15, true),
            ("0 9 15 * 1", 6, false),
            ("0 9 1-31 * 1", 6, true),
            ("0 9 15 * 0-7", 6, true),
            ("0 9 */2,2 * 1", 2, false),
            ("0 9 2,*/2 * 1", 2, true),
            ("0 9 */2,2 * 1", 12, false),
            ("0 9 2,*/2 * 1", 12, true),
            ("0 9 *,1 * 1", 6, false),
            ("0 9 1,* * 1", 6, true),
            ("0 9 15 * *,1", 6, false),
            ("0 9 15 * 1,*", 6, true),
            ("0 9 15 * */2,MON", 5, false),
            ("0 9 15 * MON,*/2", 5, true),
            ("0 9 */2 * */2", 1, true),
            ("0 9 */2 * */2", 5, false),
            ("0 9 */2 * */2", 6, false),
            ("0 9 * * 7", 4, true),
            ("0 9 * * 0", 4, true),
            ("0 9 */1 * MON", 5, true),
            ("0 9 */1 * MON", 6, false),
        ] {
            let at = Local.with_ymd_and_hms(2026, 1, day, 9, 0, 0).unwrap();
            assert_eq!(
                Cron::parse(expr).unwrap().matches(&at),
                expected,
                "{expr}, January {day}"
            );
        }
        for day in 1..=31 {
            let at = Local.with_ymd_and_hms(2026, 1, day, 9, 0, 0).unwrap();
            for (bare, step) in [
                ("0 9 * * 1", "0 9 */1 * 1"),
                ("0 9 15 * *", "0 9 15 * */1"),
                ("0 9 * * *", "0 9 */1 * */1"),
            ] {
                assert_eq!(
                    Cron::parse(bare).unwrap().matches(&at),
                    Cron::parse(step).unwrap().matches(&at)
                );
            }
        }
    }

    #[test]
    fn wildcard_step_first_run_and_previews_agree_without_rewriting_expression() {
        let now = Local.with_ymd_and_hms(2026, 1, 4, 8, 0, 0).unwrap();
        let expr = "0 9 */1 * 1";
        assert_eq!(to_cron(expr).unwrap(), expr);
        let runs = next_runs(expr, now, 3).unwrap();
        assert_eq!(
            runs.iter().map(|at| at.day()).collect::<Vec<_>>(),
            [5, 12, 19]
        );
        let mut previous = now;
        for expected in runs {
            assert_eq!(first_run(Some(expr), None, previous).unwrap(), expected);
            previous = expected;
        }
        let runs = next_runs("0 9 */2 * 1", now, 2).unwrap();
        assert_eq!(runs.iter().map(|at| at.day()).collect::<Vec<_>>(), [5, 19]);
    }

    #[test]
    fn plain_english_schedules_become_cron() {
        for (text, cron) in [
            ("every weekday at 9am", "0 9 * * 1-5"),
            ("Every 2 hours", "0 */2 * * *"),
            ("every 15 minutes", "*/15 * * * *"),
            ("daily at 18:30", "30 18 * * *"),
            ("hourly", "0 * * * *"),
            ("every monday and friday at noon", "0 12 * * 1,5"),
            ("every weekend at 8:15 pm", "15 20 * * 0,6"),
            ("0 3 * * *", "0 3 * * *"),
            ("@daily", "@daily"),
        ] {
            assert_eq!(to_cron(text).unwrap(), cron, "{text}");
        }
        assert!(to_cron("whenever you feel like it").is_err());
        assert!(to_cron("every day at 25:00").is_err());
    }

    #[test]
    fn next_runs_and_missed_firings() {
        let now = Local.with_ymd_and_hms(2026, 10, 29, 8, 0, 0).unwrap();
        let next = next_runs("0 9 * * 1-5", now, 3).unwrap();
        assert_eq!(next.len(), 3);
        assert_eq!(next[0].format("%a %H:%M").to_string(), "Thu 09:00");
        assert_eq!(next[2].format("%a %H:%M").to_string(), "Mon 09:00");
        let due = Local.with_ymd_and_hms(2026, 10, 29, 3, 0, 0).unwrap();
        assert_eq!(missed_since("0 * * * *", due, now), 5);
    }
}

#[cfg(test)]
#[path = "job_input_tests.rs"]
mod input_schedule_tests;
