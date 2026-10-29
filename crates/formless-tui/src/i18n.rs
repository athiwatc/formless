//! UI strings by language (SPEC §10.8). The catalog is keyed by the
//! English text, so a string is translated by wrapping it in [`tr`] and
//! adding a row to a language's table; anything not in the table shows in
//! English. `tui.language` picks the language: `auto` follows the
//! system locale (`LC_ALL`, `LC_MESSAGES`, `LANG`).

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En = 0,
    Th = 1,
}

/// Languages with a catalog, as `tui.language` values.
pub const LANGUAGES: &[&str] = &["auto", "en", "th"];

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// The language a `tui.language` value means here.
pub fn resolve(setting: &str) -> Lang {
    match setting.trim().to_ascii_lowercase().as_str() {
        "th" => Lang::Th,
        "en" => Lang::En,
        _ => {
            let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
                .iter()
                .filter_map(|k| std::env::var(k).ok())
                .find(|v| !v.is_empty())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if locale.starts_with("th") {
                Lang::Th
            } else {
                Lang::En
            }
        }
    }
}

pub fn set(setting: &str) {
    CURRENT.store(resolve(setting) as u8, Ordering::Relaxed);
}

thread_local! {
    /// Tests pick a language per thread, so parallel tests don't see it.
    static OVERRIDE: std::cell::Cell<Option<Lang>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with `lang` on this thread only (tests).
#[cfg(test)]
pub fn with_lang<T>(lang: Lang, f: impl FnOnce() -> T) -> T {
    OVERRIDE.with(|o| o.set(Some(lang)));
    let out = f();
    OVERRIDE.with(|o| o.set(None));
    out
}

pub fn current() -> Lang {
    if let Some(l) = OVERRIDE.with(|o| o.get()) {
        return l;
    }
    if CURRENT.load(Ordering::Relaxed) == 1 {
        Lang::Th
    } else {
        Lang::En
    }
}

/// `s` in the current language (English when there is no translation).
pub fn tr(s: &'static str) -> &'static str {
    match current() {
        Lang::En => s,
        Lang::Th => TH.iter().find(|(en, _)| *en == s).map_or(s, |(_, th)| th),
    }
}

/// [`tr`] for text that is not `'static` (labels built at run time).
pub fn tr_str(s: &str) -> String {
    match current() {
        Lang::En => s.to_string(),
        Lang::Th => TH
            .iter()
            .find(|(en, _)| *en == s)
            .map_or_else(|| s.to_string(), |(_, th)| th.to_string()),
    }
}

/// Thai. Keep the English side exactly as the code writes it.
const TH: &[(&str, &str)] = &[
    // Status bar and activity.
    ("ready", "พร้อม"),
    ("running", "กำลังทำงาน"),
    ("no session", "ไม่มีเซสชัน"),
    ("waiting for model", "รอโมเดล"),
    ("thinking", "กำลังคิด"),
    ("responding", "กำลังตอบ"),
    ("working", "กำลังทำงาน"),
    ("provider stalled", "ผู้ให้บริการไม่ตอบสนอง"),
    ("awaiting your approval", "รอการอนุมัติจากคุณ"),
    ("awaiting your answer", "รอคำตอบจากคุณ"),
    ("Esc to stop", "Esc เพื่อหยุด"),
    ("retrying in", "ลองใหม่ใน"),
    ("reconnecting to the daemon…", "กำลังเชื่อมต่อเดมอนใหม่…"),
    // Command groups (the palette and /help).
    ("Session", "เซสชัน"),
    ("Run control", "ควบคุมการทำงาน"),
    ("Tools", "เครื่องมือ"),
    ("Integrations", "การเชื่อมต่อ"),
    ("Settings", "การตั้งค่า"),
    ("App", "แอป"),
    ("Keys", "ปุ่มลัด"),
    // Common command descriptions.
    (
        "start a new Session, optionally from a template",
        "เริ่มเซสชันใหม่ (จากเทมเพลต NAME)",
    ),
    ("list and switch Sessions", "ดูและสลับเซสชัน"),
    ("fork the current Session", "แยกเซสชันปัจจุบัน"),
    ("search all Sessions", "ค้นหาทุกเซสชัน"),
    (
        "edit your last message and retry from there",
        "แก้ข้อความล่าสุดแล้วลองใหม่จากตรงนั้น",
    ),
    ("stop the current Run (Esc)", "หยุดการทำงานปัจจุบัน (Esc)"),
    (
        "continue an interrupted or paused Run",
        "ทำงานต่อจากที่ถูกขัดจังหวะหรือหยุดไว้",
    ),
    (
        "drop an interrupted or paused Run",
        "ทิ้งงานที่ถูกขัดจังหวะหรือหยุดไว้",
    ),
    (
        "re-run your last message, optionally on another model; the old answer stays in a fork",
        "ส่งข้อความล่าสุดอีกครั้ง (เลือกโมเดลอื่นได้) คำตอบเดิมเก็บไว้ในเซสชันที่แยกออกมา",
    ),
    (
        "cap the current (or next) Run at N steps; it pauses there",
        "จำกัดจำนวนขั้นของงานนี้ (หรืองานถัดไป) ที่ N แล้วหยุดรอ",
    ),
    (
        "rate the last answer 👍 (feeds Run metrics and routing)",
        "ให้คะแนนคำตอบล่าสุด 👍",
    ),
    (
        "rate the last answer 👎 (feeds Run metrics and routing)",
        "ให้คะแนนคำตอบล่าสุด 👎",
    ),
    (
        "the files the last Run changed, with diffs",
        "ไฟล์ที่งานล่าสุดแก้ไข พร้อมส่วนที่เปลี่ยน",
    ),
    (
        "put the last Run's file changes back (one file, or all)",
        "ย้อนการแก้ไขไฟล์ของงานล่าสุด (ไฟล์เดียวหรือทั้งหมด)",
    ),
    (
        "step mode: pause before each Tool call (Enter run, e edit, s skip)",
        "โหมดทีละขั้น: หยุดก่อนเรียกเครื่องมือแต่ละครั้ง (Enter ทำ, e แก้, s ข้าม)",
    ),
    (
        "the same prompt on two models, side by side with time and cost",
        "ถามคำถามเดียวกันกับสองโมเดล เทียบคำตอบ เวลา และค่าใช้จ่าย",
    ),
    (
        "the messages queued for after this Run: edit, remove or reorder them",
        "ข้อความที่รอส่งหลังงานนี้: แก้ ลบ หรือจัดลำดับ",
    ),
    (
        "expand or fold the last long Run's \"What I did\"",
        "เปิดหรือพับสรุป \"สิ่งที่ทำ\" ของงานล่าสุด",
    ),
    (
        "pick the model and effort (fuzzy search)",
        "เลือกโมเดลและระดับการคิด",
    ),
    (
        "the daemon: memory, CPU, database, running work",
        "เดมอน: หน่วยความจำ CPU ฐานข้อมูล งานที่กำลังทำ",
    ),
    (
        "disk used by Sessions, artifacts, logs, backups",
        "พื้นที่ดิสก์ที่เซสชัน ไฟล์ผลลัพธ์ บันทึก และสำรองข้อมูลใช้",
    ),
    (
        "what clean-up would free; `run` frees it",
        "ดูว่าการล้างข้อมูลจะคืนพื้นที่เท่าไร; `run` เพื่อล้างจริง",
    ),
    ("back up the database now", "สำรองฐานข้อมูลตอนนี้"),
    (
        "all commands and keys (also ? on an empty input)",
        "คำสั่งและปุ่มลัดทั้งหมด (หรือกด ? ตอนช่องพิมพ์ว่าง)",
    ),
    (
        "quit the TUI (the daemon keeps running)",
        "ออกจาก TUI (เดมอนยังทำงานต่อ)",
    ),
    // Banners and blocks.
    ("Plan", "แผน"),
    ("Plan done", "แผนเสร็จแล้ว"),
    ("Queued for after this Run", "รอส่งหลังงานนี้"),
    ("What I did", "สิ่งที่ทำ"),
    ("Step mode · about to call", "โหมดทีละขั้น · กำลังจะเรียก"),
    (
        "Enter run it · e edit the arguments · s skip it · a run the rest · Esc stop the Run",
        "Enter ทำ · e แก้อาร์กิวเมนต์ · s ข้าม · a ทำที่เหลือทั้งหมด · Esc หยุดงาน",
    ),
    ("The last Run was", "งานล่าสุด"),
    (
        "Enter (empty input) or /resume retries it · /abandon closes it",
        "Enter (ช่องว่าง) หรือ /resume เพื่อลองใหม่ · /abandon เพื่อปิด",
    ),
];

/// Thai short weekdays, Sunday first.
const TH_DAYS: [&str; 7] = ["อา.", "จ.", "อ.", "พ.", "พฤ.", "ศ.", "ส."];
/// Thai short months.
const TH_MONTHS: [&str; 12] = [
    "ม.ค.",
    "ก.พ.",
    "มี.ค.",
    "เม.ย.",
    "พ.ค.",
    "มิ.ย.",
    "ก.ค.",
    "ส.ค.",
    "ก.ย.",
    "ต.ค.",
    "พ.ย.",
    "ธ.ค.",
];

/// A message time in the current language: `now`/`2m`/`3h`, a weekday
/// and time, or a day, month and time.
pub fn stamp(kind: Stamp, at: chrono::DateTime<chrono::Local>) -> String {
    use chrono::{Datelike, Timelike};
    match (current(), kind) {
        (Lang::En, Stamp::Now) => "now".into(),
        (Lang::En, Stamp::Minutes(m)) => format!("{m}m"),
        (Lang::En, Stamp::Hours(h)) => format!("{h}h"),
        (Lang::En, Stamp::Weekday) => at.format("%a %H:%M").to_string(),
        (Lang::En, Stamp::Date) => at.format("%-d %b %H:%M").to_string(),
        (Lang::Th, Stamp::Now) => "เมื่อกี้".into(),
        (Lang::Th, Stamp::Minutes(m)) => format!("{m} นาที"),
        (Lang::Th, Stamp::Hours(h)) => format!("{h} ชม."),
        (Lang::Th, Stamp::Weekday) => format!(
            "{} {:02}:{:02}",
            TH_DAYS[at.weekday().num_days_from_sunday() as usize],
            at.hour(),
            at.minute()
        ),
        (Lang::Th, Stamp::Date) => format!(
            "{} {} {:02}:{:02}",
            at.day(),
            TH_MONTHS[at.month0() as usize],
            at.hour(),
            at.minute()
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stamp {
    Now,
    Minutes(i64),
    Hours(i64),
    Weekday,
    Date,
}

/// A whole number with the locale's digit grouping (`12,345`).
pub fn number(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn thai_is_used_when_chosen_and_english_fills_gaps() {
        let at = chrono::Local
            .with_ymd_and_hms(2026, 3, 3, 14, 2, 0)
            .unwrap();
        with_lang(Lang::Th, || {
            assert_eq!(tr("ready"), "พร้อม");
            assert_eq!(tr_str("thinking"), "กำลังคิด");
            assert_eq!(tr("not in the catalog"), "not in the catalog");
            assert_eq!(stamp(Stamp::Date, at), "3 มี.ค. 14:02");
            assert_eq!(stamp(Stamp::Minutes(5), at), "5 นาที");
        });
        with_lang(Lang::En, || {
            assert_eq!(tr("ready"), "ready");
            assert_eq!(stamp(Stamp::Date, at), "3 Mar 14:02");
        });
        assert_eq!(resolve("th"), Lang::Th);
        assert_eq!(resolve("en"), Lang::En);
        assert_eq!(number(1234567), "1,234,567");
        assert_eq!(number(-12), "-12");
        // Every Thai row translates a distinct English string.
        let mut keys: Vec<&str> = TH.iter().map(|(en, _)| *en).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), TH.len());
    }
}
