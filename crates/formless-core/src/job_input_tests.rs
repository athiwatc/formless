//! Human-readable recurring inputs retain the existing numeric cron contract.
use super::*;

#[test]
fn cron_names_match_numeric_singletons_lists_ranges_and_steps() {
    for (named, numeric) in [
        ("0 9 * * MON-FRI", "0 9 * * 1-5"),
        ("0 9 * * mon,wed,fri", "0 9 * * 1,3,5"),
        ("0 0 1 JAN *", "0 0 1 1 *"),
        ("30 8 * * Sun", "30 8 * * 0"),
        ("0 0 * * SUN", "0 0 * * 7"),
        ("0 0 * JAN-MAR *", "0 0 * 1-3 *"),
        ("0 0 * jan,jul *", "0 0 * 1,7 *"),
        ("0 0 * JAN-MAR/2 MON-FRI/2", "0 0 * 1-3/2 1-5/2"),
        ("0 0 * JAN,6,dec SUN,3,SAT", "0 0 * 1,6,12 0,3,6"),
        ("0 0 * FEB/3 TUE/2", "0 0 * 2/3 2/2"),
    ] {
        assert_eq!(
            Cron::parse(named).unwrap(),
            Cron::parse(numeric).unwrap(),
            "{named}"
        );
        assert_eq!(
            to_cron(named).unwrap(),
            named,
            "valid named cron remains verbatim"
        );
    }
    for (month, number) in [
        ("Jan", 1),
        ("Feb", 2),
        ("Mar", 3),
        ("Apr", 4),
        ("May", 5),
        ("Jun", 6),
        ("Jul", 7),
        ("Aug", 8),
        ("Sep", 9),
        ("Oct", 10),
        ("Nov", 11),
        ("Dec", 12),
    ] {
        assert_eq!(
            Cron::parse(&format!("0 0 1 {month} *")).unwrap(),
            Cron::parse(&format!("0 0 1 {number} *")).unwrap()
        );
    }
    for (day, number) in [
        ("Sun", 0),
        ("Mon", 1),
        ("Tue", 2),
        ("Wed", 3),
        ("Thu", 4),
        ("Fri", 5),
        ("Sat", 6),
    ] {
        assert_eq!(
            Cron::parse(&format!("0 0 * * {day}")).unwrap(),
            Cron::parse(&format!("0 0 * * {number}")).unwrap()
        );
    }
}

#[test]
fn invalid_names_and_range_endpoints_retain_field_diagnostics_for_user_and_agent() {
    for (cron, field) in [
        ("bad-9 0 * * *", "minute"),
        ("0 bad-9 * * *", "hour"),
        ("0 0 bad-20 * *", "day"),
        ("0 0 1 JAN-NOP *", "month"),
        ("0 0 1 JANUARY *", "month"),
        ("0 9 * * MON-NOP", "weekday"),
        ("0 9 * * FRI-MON", "weekday"),
        ("0 0 1 JAN-MAR/0 *", "month"),
        ("0 0 * * MON/0", "weekday"),
    ] {
        for error in [Cron::parse(cron).unwrap_err(), to_cron(cron).unwrap_err()] {
            let error = error.to_string();
            assert!(error.contains(field), "{cron}: {error}");
            assert!(!error.contains("invalid digit found"), "{cron}: {error}");
        }
    }
}

#[test]
fn dotted_clock_suffixes_are_case_insensitive_without_accepting_numeric_dots() {
    for (clock, expected) in [
        ("9 a.m.", (9, 0)),
        ("9a.m", (9, 0)),
        ("9 am.", (9, 0)),
        ("9AM", (9, 0)),
        ("9 p.m.", (21, 0)),
        ("9p.m", (21, 0)),
        ("9 pm.", (21, 0)),
        ("9PM", (21, 0)),
        ("9:30 A.M.", (9, 30)),
        ("9:30 p.m.", (21, 30)),
        ("12 a.m.", (0, 0)),
        ("12 p.m.", (12, 0)),
        ("18:30", (18, 30)),
    ] {
        assert_eq!(parse_time(clock), Some(expected), "{clock}");
    }
    for bad in [
        "9.30am",
        "9..30a.m.",
        "9a..m.",
        "9q.m.",
        "13p.m.",
        "9:60a.m.",
        "24:00",
    ] {
        assert!(parse_time(bad).is_none(), "{bad}");
    }
}

#[test]
fn natural_dotted_times_and_yearly_phrases_are_parsed_before_cron_diagnostics() {
    for (phrase, expected) in [
        ("every weekday at 9 a.m.", "0 9 * * 1-5"),
        ("daily at 9 p.m.", "0 21 * * *"),
        ("Every Monday at 9:30 A.M.", "30 9 * * 1"),
        ("daily at 12 a.m.", "0 0 * * *"),
        ("daily at 12 p.m.", "0 12 * * *"),
        ("daily at 9 am.", "0 9 * * *"),
        ("daily at 9 pm.", "0 21 * * *"),
        ("yearly", "0 0 1 1 *"),
        ("annually", "0 0 1 1 *"),
        ("year", "0 0 1 1 *"),
        ("every year", "0 0 1 1 *"),
        ("every year at 9am", "0 9 1 1 *"),
        ("Yearly at 9:30 P.M.", "30 21 1 1 *"),
        ("annually at noon", "0 12 1 1 *"),
    ] {
        assert_eq!(to_cron(phrase).unwrap(), expected, "{phrase}");
        assert!(Cron::parse(expected).is_ok());
    }
    for bad in ["yearly at 25:00", "every year at 9.30am", "daily at 9a..m."] {
        assert!(to_cron(bad).is_err(), "{bad}");
    }
}

#[test]
fn yearly_aliases_and_named_calendar_fields_choose_actual_occurrences() {
    let midnight = Cron::parse("0 0 1 1 *").unwrap();
    let after = Local.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let next = Local.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap();
    for alias in ["@yearly", "@annually"] {
        let parsed = Cron::parse(alias).unwrap();
        assert_eq!(parsed, midnight);
        assert_eq!(parsed.next_after(after), Some(next));
        assert_eq!(to_cron(alias).unwrap(), alias);
    }
    let now = Local.with_ymd_and_hms(2026, 12, 31, 12, 0, 0).unwrap();
    let morning = Local.with_ymd_and_hms(2027, 1, 1, 9, 30, 0).unwrap();
    assert_eq!(next_runs("30 9 1 JAN *", now, 1).unwrap(), vec![morning]);
    // With both day fields restricted, either may match (existing cron rule).
    let january = Local.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap();
    let monday = Local.with_ymd_and_hms(2026, 1, 5, 9, 0, 0).unwrap();
    assert_eq!(
        next_runs("0 9 15 JAN MON", january, 1).unwrap(),
        vec![monday]
    );
}
