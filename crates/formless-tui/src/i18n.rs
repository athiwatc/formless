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
        Lang::Th => ONBOARDING_TH
            .iter()
            .chain(TH)
            .find(|(en, _)| *en == s)
            .map_or(s, |(_, th)| th),
    }
}

/// [`tr`] for text that is not `'static` (labels built at run time).
pub fn tr_str(s: &str) -> String {
    match current() {
        Lang::En => s.to_string(),
        Lang::Th => ONBOARDING_TH
            .iter()
            .chain(TH)
            .find(|(en, _)| *en == s)
            .map_or_else(|| s.to_string(), |(_, th)| th.to_string()),
    }
}

/// Fill catalog placeholders with values in their original order.
pub fn tr_format(template: &'static str, values: &[&str]) -> String {
    formless_protocol::fill_display_template(tr(template), values)
}

/// Thai. Keep the English side exactly as the code writes it.
const ONBOARDING_TH: &[(&str, &str)] = &[
    // First-run and setup.
    ("OLLAMA_HOST is not a supported local address; add the provider manually.", "OLLAMA_HOST ไม่ใช่ที่อยู่ในเครื่องที่รองรับ โปรดเพิ่มผู้ให้บริการด้วยตนเอง"),
    ("OLLAMA_HOST points at a remote host; add it as a provider manually.", "OLLAMA_HOST ชี้ไปที่เครื่องอื่น โปรดเพิ่มผู้ให้บริการด้วยตนเอง"),
    ("No model API answered at the local OLLAMA_HOST address. Check Ollama or add the provider manually.", "API โมเดลที่ที่อยู่ OLLAMA_HOST ในเครื่องไม่ตอบสนอง ตรวจสอบ Ollama หรือเพิ่มผู้ให้บริการด้วยตนเอง"),
    ("Ollama's model API is available but has no models. Run `ollama pull MODEL`, then retry /setup, or choose another provider.", "เชื่อมต่อ Ollama API ได้ แต่ยังไม่มีโมเดล ใช้ `ollama pull MODEL` แล้วลอง /setup อีกครั้ง หรือเลือกผู้ให้บริการอื่น"),
    ("Local model API at {} has no models. Load a model in the local server, then retry /setup, or choose another provider.", "API โมเดลในเครื่องที่ {} ยังไม่มีโมเดล โหลดโมเดลบนเซิร์ฟเวอร์แล้วลอง /setup อีกครั้ง หรือเลือกผู้ให้บริการอื่น"),
    ("Local model API at {} offers {} model(s). Press Enter to select one, or choose another provider.", "API โมเดลในเครื่องที่ {} มี {} โมเดล กด Enter เพื่อเลือกโมเดล หรือเลือกผู้ให้บริการอื่น"),
    ("{} model", "{} โมเดล"),
    ("{} models", "{} โมเดล"),
    ("  (found: {})", "  (พบ {})"),
    ("  (${} found)", "  (พบ ${})"),
    ("Can't reach {} ({}). Start the server, then press Enter to retry (Esc for another provider).", "เชื่อมต่อ {} ไม่ได้ ({}) เริ่มเซิร์ฟเวอร์แล้วกด Enter เพื่อลองใหม่ (กด Esc เพื่อเลือกผู้ให้บริการอื่น)"),
    ("{} Enter it again. ({})", "{} ป้อนอีกครั้ง ({})"),
    ("Could not list models: {}{} Type a model name, then Enter. ({})", "ดึงรายการโมเดลไม่สำเร็จ: {}{} พิมพ์ชื่อโมเดลแล้วกด Enter ({})"),
    ("Probe failed: {}{} Press Enter to pick a model again. ({})", "ตรวจสอบไม่สำเร็จ: {}{} กด Enter เพื่อเลือกโมเดลอีกครั้ง ({})"),
    ("Welcome to Formless. Choose your model provider.", "ยินดีต้อนรับสู่ Formless เลือกผู้ให้บริการโมเดล"),
    ("Saved setup found. Enter a replacement API key, or press Enter to reuse its stored key when available (Esc to choose another provider).", "พบการตั้งค่าที่บันทึกไว้ ป้อนคีย์ API ใหม่ หรือกด Enter เพื่อใช้คีย์ที่บันทึกไว้ถ้ามี (กด Esc เพื่อเลือกผู้ให้บริการอื่น)"),
    ("Welcome back. Enter your provider base URL.", "ยินดีต้อนรับกลับมา ป้อน URL พื้นฐานของผู้ให้บริการ"),
    ("Enter your provider base URL (any OpenAI-compatible endpoint).", "ป้อน URL พื้นฐานของผู้ให้บริการ (ปลายทางใดก็ได้ที่รองรับ OpenAI API)"),
    ("Enter your API key (stored in the OS keychain).", "ป้อนคีย์ API ระบบจะเก็บคีย์ไว้ในพวงกุญแจของระบบปฏิบัติการ"),
    ("The API key contains whitespace inside it. Remove it before continuing.", "คีย์ API มีช่องว่างอยู่ภายใน โปรดลบช่องว่างก่อนดำเนินการต่อ"),
    ("Press Enter to use the highlighted provider.", "กด Enter เพื่อใช้ผู้ให้บริการที่ไฮไลต์อยู่"),
    ("Pick a model: type to filter, Up/Down, Enter.", "เลือกโมเดล: พิมพ์เพื่อกรอง ใช้ลูกศรขึ้นหรือลง แล้วกด Enter"),
    ("The endpoint listed no models; type a model name, then Enter.", "ปลายทางไม่แสดงรายการโมเดล ให้พิมพ์ชื่อโมเดลแล้วกด Enter"),
    ("This endpoint/model supports neither native nor prompt-based tool-calling. Press Enter to pick another model.", "ปลายทางหรือโมเดลนี้ไม่รองรับการเรียกเครื่องมือทั้งแบบโดยตรงและผ่านพรอมต์ กด Enter เพื่อเลือกโมเดลอื่น"),
    ("This endpoint uses no new key. Enter to confirm removing the previous endpoint's stored key, or Esc to go back. Nothing has been saved yet.", "ปลายทางนี้ไม่ใช้คีย์ใหม่ กด Enter เพื่อยืนยันการลบคีย์ที่บันทึกไว้ของปลายทางก่อนหน้า หรือกด Esc เพื่อย้อนกลับ ยังไม่ได้บันทึกอะไร"),
    ("Choose your model provider.", "เลือกผู้ให้บริการโมเดล"),
    ("Pick a model.", "เลือกโมเดล"),
    ("Pick reasoning effort.", "เลือกระดับการใช้เหตุผล"),
    ("Probing endpoint (connectivity + streaming + tool-calling)…", "กำลังตรวจสอบปลายทาง (การเชื่อมต่อ การสตรีม และการเรียกเครื่องมือ)…"),
    ("Setup cancelled; your existing setup is unchanged.", "ยกเลิกการตั้งค่าแล้ว การตั้งค่าเดิมยังอยู่"),
    ("New here? /tour runs a 2-minute guided first task (/tour skip hides this for good).", "เพิ่งเริ่มใช้ใช่ไหม ใช้ /tour เพื่อทดลองงานแรกแบบมีคำแนะนำใน 2 นาที (ใช้ /tour skip เพื่อซ่อนคำแนะนำนี้)"),
    ("tour skipped; /tour starts it any time", "ข้ามการแนะนำแล้ว ใช้ /tour เพื่อเริ่มได้ทุกเมื่อ"),
    ("Use /tour to start, /tour skip to hide the offer, and /tour cleanup to archive the tour and retire its sample.", "ใช้ /tour เพื่อเริ่ม /tour skip เพื่อซ่อนคำแนะนำ และ /tour cleanup เพื่อเก็บเซสชันและเลิกใช้เครื่องมือตัวอย่าง"),
    ("Build a Tool named {} that counts the words, lines and characters of a text. Reuse it if it already exists. Then call it on: \"The quick brown fox jumps over the lazy dog.\"", "สร้างเครื่องมือชื่อ {} เพื่อนับจำนวนคำ บรรทัด และอักขระในข้อความ หากมีอยู่แล้วให้ใช้เครื่องมือเดิม แล้วเรียกใช้กับข้อความนี้: \"The quick brown fox jumps over the lazy dog.\""),
    ("Use the existing Tool {} to count the words, lines and characters of: \"The quick brown fox jumps over the lazy dog.\"", "ใช้เครื่องมือ {} ที่มีอยู่เพื่อนับจำนวนคำ บรรทัด และอักขระในข้อความนี้: \"The quick brown fox jumps over the lazy dog.\""),
    ("Tour 1/3 · Press Enter to run the sample task in this Session. /tour cleanup archives the tour and retires its sample.", "แนะนำ 1/3 · กด Enter เพื่อทำงานตัวอย่างในเซสชันนี้ ใช้ /tour cleanup เพื่อเก็บเซสชันและเลิกใช้เครื่องมือตัวอย่าง"),
    ("Tour 2/3 · The agent built the sample Tool. Open the Tools view with Tab to inspect its code and versions.", "แนะนำ 2/3 · เอเจนต์สร้างเครื่องมือตัวอย่างแล้ว กด Tab เพื่อเปิดมุมมองเครื่องมือและดูโค้ดกับเวอร์ชัน"),
    ("Tour 2/3 · The agent reused the sample Tool. Open the Tools view with Tab to inspect its code and versions.", "แนะนำ 2/3 · เอเจนต์ใช้เครื่องมือตัวอย่างเดิม กด Tab เพื่อเปิดมุมมองเครื่องมือและดูโค้ดกับเวอร์ชัน"),
    ("Tour 3/3 · Done. The agent used its sample Tool. /help lists commands and keys.", "แนะนำ 3/3 · เสร็จแล้ว เอเจนต์ใช้เครื่องมือตัวอย่างแล้ว ดูคำสั่งและปุ่มลัดด้วย /help"),
    ("Tour guidance paused. Return to the Guided tour Session or type /tour to continue.", "หยุดคำแนะนำชั่วคราว กลับไปที่เซสชัน Guided tour หรือพิมพ์ /tour เพื่อดำเนินการต่อ"),
    ("Tour guidance stopped. Use /tour to start a new sample task.", "หยุดคำแนะนำแล้ว ใช้ /tour เพื่อเริ่มงานตัวอย่างใหม่"),
    ("The tour send was unconfirmed. Check the Guided tour Session before retrying.", "ยังไม่ได้รับการยืนยันการส่งงานตัวอย่าง ตรวจสอบเซสชัน Guided tour ก่อนลองใหม่"),
    ("The tour send did not start a new Run. Use /tour to try again.", "การส่งงานตัวอย่างไม่ได้เริ่มการรันใหม่ ใช้ /tour เพื่อลองอีกครั้ง"),
    ("The tour built its Tool but did not use it successfully. Check the transcript for details.", "สร้างเครื่องมือตัวอย่างแล้วแต่ใช้งานไม่สำเร็จ ดูรายละเอียดในประวัติการสนทนา"),
    ("The agent answered without building or using the sample Tool. Some models answer directly. Try /tour with a different model.", "เอเจนต์ตอบโดยไม่ได้สร้างหรือใช้เครื่องมือตัวอย่าง บางโมเดลตอบโดยตรง ลองใช้ /tour กับโมเดลอื่น"),
    ("The tour Run paused. Check the Run error in this Session, or use /tour to try again.", "การรันงานตัวอย่างหยุดชั่วคราว ตรวจสอบข้อผิดพลาดในเซสชันนี้ หรือใช้ /tour เพื่อลองอีกครั้ง"),
    ("The tour Run was interrupted. Use /tour to try again.", "การรันงานตัวอย่างถูกขัดจังหวะ ใช้ /tour เพื่อลองอีกครั้ง"),
    ("The tour Run ended with status {}. Check the transcript for details.", "การรันงานตัวอย่างสิ้นสุดด้วยสถานะ {} ดูรายละเอียดในประวัติการสนทนา"),
    ("Tour Sessions archived: {}.", "เก็บเซสชันแนะนำแล้ว {} เซสชัน"),
    ("The sample Tool was retired.", "เลิกใช้เครื่องมือตัวอย่างแล้ว"),
    ("The sample Tool was kept because it was modified or pinned.", "เก็บเครื่องมือตัวอย่างไว้เนื่องจากมีการแก้ไขหรือปักหมุด"),
    ("⚠ The endpoint didn't stream its test reply: answers may arrive all at once, and a long one can look stuck until it ends.", "⚠ ปลายทางไม่ได้สตรีมคำตอบทดสอบ คำตอบอาจปรากฏพร้อมกันทั้งหมด และคำตอบยาวอาจดูเหมือนค้างจนกว่าจะเสร็จ"),
    ("Approvals are on: a Tool asks before it uses files, the network or programs.", "เปิดการอนุมัติแล้ว เครื่องมือจะขออนุญาตก่อนใช้ไฟล์ เครือข่าย หรือโปรแกรม"),
    ("Tools run without asking (approvals are off). To be asked first, set security.secure = true in config.toml or start with `formless --secure`.", "เครื่องมือทำงานโดยไม่ถาม (ปิดการอนุมัติ) หากต้องการให้ถามก่อน ให้ตั้ง security.secure = true ใน config.toml หรือเริ่มด้วย `formless --secure`"),
    ("Provider", "ผู้ให้บริการ"),
    ("API key", "คีย์ API"),
    ("Model", "โมเดล"),
    ("Reasoning effort", "ระดับการใช้เหตุผล"),
    ("Tool-calling check", "ตรวจสอบการเรียกเครื่องมือ"),
    ("Base URL", "URL พื้นฐาน"),
    ("Custom URL…", "URL กำหนดเอง…"),
    ("          Ctrl+R hide · paste works", "          Ctrl+R ซ่อน · วางคีย์ได้"),
    ("          Ctrl+R show · paste works", "          Ctrl+R แสดง · วางคีย์ได้"),
    ("default (provider)", "ค่าเริ่มต้น (ผู้ให้บริการ)"),
    ("Enter continue · Esc back (at Provider: cancel) · Ctrl+C quit", "Enter ดำเนินการต่อ · Esc ย้อนกลับ (ที่หน้าผู้ให้บริการ: ยกเลิก) · Ctrl+C ออก"),
    ("Enter continue · Esc back · Ctrl+C quit", "Enter ดำเนินการต่อ · Esc ย้อนกลับ · Ctrl+C ออก"),
    ("More: formless profiles list", "ดูเพิ่มเติมด้วย formless profiles list"),
    ("First approval? Tools run in a sandbox with no access at all; each capability above", "อนุมัติครั้งแรกใช่ไหม เครื่องมือทำงานแยกและไม่มีสิทธิ์เข้าถึงอะไรโดยค่าเริ่มต้น"),
    ("(files, network, programs…) is something this Tool asks to use. y lets it keep using", "รายการด้านบนคือสิทธิ์ที่เครื่องมือนี้ขอ เช่น ไฟล์ เครือข่าย หรือโปรแกรม กด y เพื่ออนุญาตต่อเนื่อง"),
    ("them, o allows just this call, s for this Session, n says no. Change your mind any", "กด o เพื่ออนุญาตครั้งเดียว กด s สำหรับเซสชันนี้ หรือกด n เพื่อปฏิเสธ คุณเปลี่ยนใจได้ทุกเมื่อ"),
    ("time: /approvals lists what you allowed, /revoke takes it back.", "ใช้ /approvals เพื่อดูสิทธิ์ที่อนุญาต และ /revoke เพื่อถอนอนุญาต"),
    (" always    ", " อนุญาตเสมอ    "),
    (" once    ", " ครั้งเดียว    "),
    (" this Session    ", " เซสชันนี้    "),
    (" deny  ", " ปฏิเสธ  "),
    ("This profile needs setup. {}", "โปรไฟล์นี้ต้องตั้งค่า {}"),
    ("Welcome back: you were setting up {}. {}", "ยินดีต้อนรับกลับมา คุณกำลังตั้งค่า {} อยู่ {}"),
    (" Get one at {}", " รับคีย์ได้ที่ {}"),
    ("Found ${}: press Enter to use it, or type a different key.", "พบ ${} กด Enter เพื่อใช้คีย์นี้ หรือป้อนคีย์อื่น"),
    ("Fetching models from {}…", "กำลังดึงรายการโมเดลจาก {}…"),
    ("Found {}. {}", "พบ {} {}"),
    ("   step {} of 5 · {}", "   ขั้นที่ {} จาก 5 · {}"),
    (" ◇ Formless setup · profile: {} ", " ◇ ตั้งค่า Formless · โปรไฟล์: {} "),
    ("Cancelled pending request. {}", "ยกเลิกคำขอที่รออยู่แล้ว {}"),
    ("Saving config failed: {}", "บันทึกการตั้งค่าไม่สำเร็จ: {}"),
];

/// Thai UI copy outside onboarding.
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

#[cfg(test)]
#[test]
fn onboarding_copy_has_thai_entries_and_uses_catalog_calls() {
    let tui = concat!(include_str!("lib.rs"), include_str!("tour.rs"));
    let core = include_str!("../../formless-core/src/local_servers.rs");
    let missing: Vec<_> = ONBOARDING_TH
        .iter()
        .filter_map(|(english, thai)| {
            let literal = serde_json::to_string(english).unwrap();
            let direct = tui.match_indices(&literal).any(|(offset, _)| {
                let prefix = tui[..offset].trim_end();
                prefix.ends_with("tr(") || prefix.ends_with("tr_format(")
            });
            let through_offer = core.contains(&literal) && tui.contains("offer_with(tr_format)");
            let discovery_template = english
                .replace("OLLAMA_HOST", "{env}")
                .replace("Ollama", "{app}");
            let discovery_hint = english.contains("OLLAMA_HOST")
                && core.contains(&serde_json::to_string(&discovery_template).unwrap())
                && tui.contains("discovery.hint.as_deref().map(i18n::tr_str)");
            (!(direct || through_offer || discovery_hint)
                || english.matches("{}").count() != thai.matches("{}").count()
                || !thai.chars().any(|c| ('\u{e00}'..='\u{e7f}').contains(&c)))
            .then_some(*english)
        })
        .collect();
    assert!(
        missing.is_empty(),
        "Untranslated onboarding text {missing:?}"
    );
}
