//! The homepage: a status board over chapter -> lesson -> node; mirrors `extensions/homepage.ts`.
//!
//! Nothing here is stored: the board is a join of the plan (`chapter.rs`), the artifacts on disk
//! (`courses/<course>/lessons/*.html`) and AnkiConnect. `fetchDeckStatus` is server-only (#8).

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::anki::{self, AnkiStatus, DEFAULT_ANKI_URL};
use crate::chapter::{
    chapter_slug, chapter_title, curriculum_file, js_number, match_curriculum, read_curriculum,
    CurriculumMatch, LessonSummary, PlannedLesson,
};
use crate::clock;
use crate::data_root;
use crate::identity::{APP_NAME, DECK_ROOT};
use crate::slug::js_trim;

/// Pedagogical tier -> dot CSS class (`TIER_CLASS` in homepage.ts; unknown tiers draw as derivation).
const TIER_CLASSES: [(&str, &str); 3] = [
    ("unconditional-truth", "truth"),
    ("axiom", "axiom"),
    ("derivation", "deriv"),
];

fn tier_class(tier: &str) -> &'static str {
    TIER_CLASSES
        .iter()
        .find(|(name, _)| *name == tier)
        .map(|(_, class)| *class)
        .unwrap_or("deriv")
}

/// Escapes a string for literal use in HTML. Port of `escapeHtml` in `html-lesson.ts`.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// JS `encodeURI`: keeps the reserved/unreserved ASCII set, percent-encodes everything else.
pub fn encode_uri(s: &str) -> String {
    const KEPT: &str = ";,/?:@&=+$-_.!~*'()#";
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || KEPT.contains(c) {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

/// JS `Number.prototype.toFixed(1)` for 0..=100: round half-up on the exact binary value.
pub fn to_fixed_1(x: f64) -> String {
    // 60 decimals are exact for every f64 in this range, so the digit we round on is the real one.
    let exact = format!("{x:.60}");
    let (integer, fraction) = exact.split_once('.').unwrap_or((exact.as_str(), ""));
    let mut first = fraction.as_bytes().first().map_or(0, |b| b - b'0');
    let second = fraction.as_bytes().get(1).map_or(0, |b| b - b'0');
    let mut integer = integer.parse::<u64>().unwrap_or(0);
    if second >= 5 {
        first += 1;
        if first == 10 {
            first = 0;
            integer += 1;
        }
    }
    format!("{integer}.{first}")
}

/// Reads the data island out of an artifact. No island means a hand-authored lesson, not an error.
pub fn parse_lesson_document(raw: &str) -> Result<Option<Value>, String> {
    const OPEN: &str = "<script type=\"application/json\" id=\"lesson-data\">";
    let Some(start) = raw.find(OPEN).map(|i| i + OPEN.len()) else {
        return Ok(None);
    };
    let Some(length) = raw[start..].find("</script>") else {
        return Ok(None);
    };
    match serde_json::from_str::<Value>(js_trim(&raw[start..start + length])) {
        Ok(Value::Object(document)) => Ok(Some(Value::Object(document))),
        Ok(_) => Ok(None),
        Err(error) => Err(format!("Malformed #lesson-data island: {error}")),
    }
}

/// Summarizes one artifact. Every field is read leniently: a missing key is an empty one.
pub fn summarize_lesson(
    courses_dir: &Path,
    course: &str,
    filename: &str,
    doc: &Value,
) -> LessonSummary {
    let metadata = doc.get("metadata");
    let field = |key: &str| metadata.and_then(|m| m.get(key));
    let lesson_number = field("lessonNumber")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_f64().map(|number| number.trunc() as i64))
        })
        .unwrap_or(0);
    let slug = field("slug").and_then(Value::as_str).unwrap_or_default();
    let title = match field("title").and_then(Value::as_str) {
        Some(title) if !title.is_empty() => title,
        _ => field("topic").and_then(Value::as_str).unwrap_or_default(),
    };
    let nodes = doc
        .get("nodes")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    LessonSummary {
        course: course.to_string(),
        lesson_number,
        slug: slug.to_string(),
        title: title.to_string(),
        filename: filename.to_string(),
        node_count: nodes.len(),
        tiers: nodes
            .iter()
            .map(|node| {
                node.get("tier")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect(),
        has_reference: courses_dir
            .join(course)
            .join("reference")
            .join(format!("{slug}.html"))
            .exists(),
    }
}

pub fn courses_dir_for(base_dir: &Path, dir: Option<&str>) -> PathBuf {
    match dir {
        Some(dir) if !dir.is_empty() => data_root::resolve_path_from(base_dir, Path::new(dir)),
        _ => base_dir.join("courses"),
    }
}

/// JS `.sort()` on directory entries: UTF-16 code units, not bytes.
fn utf16_key(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// Walks every `courses/<course>/lessons/*.html` carrying a data island. A file without one is not
/// a lesson artifact and is skipped, not guessed at.
///
/// The final order is course then lesson number. Courses are compared by UTF-16 code units: for the
/// slug directory names the engine creates (lowercase ASCII, digits, `-`) this is also
/// `localeCompare` order; mixed-case, `_` or accented names may order differently.
pub fn scan_lessons(courses_dir: &Path, warn: &mut dyn Write) -> Vec<LessonSummary> {
    let Ok(entries) = fs::read_dir(courses_dir) else {
        return Vec::new();
    };
    let mut courses: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    courses.sort_by_key(|course| utf16_key(course));

    let mut summaries = Vec::new();
    for course in courses {
        let lessons_dir = courses_dir.join(&course).join("lessons");
        let is_dir = fs::metadata(&lessons_dir).map(|m| m.is_dir()).unwrap_or(false);
        if !is_dir {
            continue;
        }
        let Ok(entries) = fs::read_dir(&lessons_dir) else {
            continue;
        };
        let mut filenames: Vec<String> = entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        filenames.sort_by_key(|filename| utf16_key(filename));

        for filename in filenames {
            if !filename.ends_with(".html") {
                continue;
            }
            let raw = match fs::read(lessons_dir.join(&filename)) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(error) => {
                    let _ = writeln!(warn, "Ignorata {course}/{filename}: {error}");
                    continue;
                }
            };
            match parse_lesson_document(&raw) {
                // One unreadable artifact must not take the whole board down: name it and go on.
                Err(message) => {
                    let _ = writeln!(warn, "Ignorata {course}/{filename}: {message}");
                }
                Ok(None) => {}
                Ok(Some(doc)) => {
                    summaries.push(summarize_lesson(courses_dir, &course, &filename, &doc));
                }
            }
        }
    }

    summaries.sort_by(|a, b| {
        utf16_key(&a.course)
            .cmp(&utf16_key(&b.course))
            .then(a.lesson_number.cmp(&b.lesson_number))
    });
    summaries
}

/// Every course that should have a deck: the plan's chapters, then course dirs already on disk.
pub fn course_slugs(
    curriculum_file: &Path,
    courses_dir: &Path,
    warn: &mut dyn Write,
) -> Vec<String> {
    let mut slugs: Vec<String> = Vec::new();
    if let Ok(curriculum) = read_curriculum(curriculum_file) {
        slugs.extend(curriculum.chapters.iter().map(chapter_slug));
    }
    slugs.extend(scan_lessons(courses_dir, warn).into_iter().map(|l| l.course));

    let mut seen = HashSet::new();
    slugs.retain(|slug| seen.insert(slug.clone()));
    slugs
}

/// `Proxi::<chapter title>`, or the directory name when the plan has no title.
pub fn course_deck_name_for(base_dir: &Path, course: &str) -> String {
    let mut title = read_curriculum(&base_dir.join("curriculum.json"))
        .ok()
        .and_then(|curriculum| {
            curriculum
                .chapters
                .iter()
                .find(|chapter| chapter_slug(chapter) == course)
                .map(chapter_title)
        })
        .unwrap_or_default();
    if title.is_empty() && course == "lezioni-singole" {
        title = "Lezioni singole".to_string();
    }
    if title.is_empty() {
        title = course.to_string();
    }
    // Anki reads `::` as a deck separator; a title with one must not create a subdeck.
    let title = title.replace("::", " ").replace(['\\', '/'], " ");
    format!("{DECK_ROOT}::{}", js_trim(&title))
}

/// The status for the board: `fetchAnkiStatus` with this module's plan scan and deck names.
pub async fn board_anki_status(
    url: &str,
    timeout: Duration,
    base_dir: &Path,
    courses_dir: &Path,
    curriculum_file: &Path,
    warn: &mut dyn Write,
) -> Result<AnkiStatus, String> {
    anki::fetch_anki_status(
        url,
        timeout,
        || course_slugs(curriculum_file, courses_dir, warn),
        |course| course_deck_name_for(base_dir, course),
    )
    .await
}

/// One dot per node: coloured for a captured node, hollow for a planned one.
fn render_dots(lesson: Option<&LessonSummary>, planned_nodes: Option<f64>) -> String {
    let inner = match lesson {
        Some(lesson) => lesson
            .tiers
            .iter()
            .map(|tier| format!("<i class=\"{}\"></i>", tier_class(tier)))
            .collect::<String>(),
        None => {
            let count = planned_nodes.unwrap_or(0.0);
            let count = if count > 0.0 { count.trunc() as usize } else { 0 };
            "<i class=\"pending\"></i>".repeat(count)
        }
    };
    if inner.is_empty() {
        String::new()
    } else {
        format!("<span class=\"dots\">{inner}</span>")
    }
}

fn render_row(planned: Option<&PlannedLesson>, lesson: Option<&LessonSummary>) -> String {
    let title = escape_html(match (lesson, planned) {
        (Some(lesson), _) => lesson.title.as_str(),
        (None, Some(planned)) => planned.title.as_str(),
        (None, None) => "",
    });
    let mark = if lesson.is_some() { "&check;" } else { "&middot;" };
    let number = match lesson {
        Some(lesson) => format!("{:04}", lesson.lesson_number),
        None => "&mdash;".to_string(),
    };
    let reference = match lesson {
        Some(lesson) if lesson.has_reference => format!(
            " <a class=\"ref\" href=\"{}\">ref</a>",
            encode_uri(&reference_href(lesson))
        ),
        _ => String::new(),
    };
    let cell_title = match lesson {
        Some(lesson) => format!(
            "<a href=\"{}\">{title}</a>{reference}",
            encode_uri(&lesson_href(lesson))
        ),
        None => title,
    };
    let count = match lesson {
        Some(lesson) if lesson.node_count == 0 => "prosa".to_string(),
        Some(lesson) if lesson.node_count == 1 => "1 nodo".to_string(),
        Some(lesson) => format!("{} nodi", lesson.node_count),
        None => planned
            .and_then(|p| p.planned_nodes)
            .filter(|nodes| *nodes != 0.0)
            .map(|nodes| format!("{} nodi previsti", js_number(nodes)))
            .unwrap_or_else(|| "da fare".to_string()),
    };
    let class = if lesson.is_some() { "" } else { "is-todo" };
    format!(
        "<tr class=\"{class}\"><td class=\"mark\">{mark}</td><td class=\"num\">{number}</td><td class=\"title\">{cell_title}</td><td class=\"r\">{}</td><td class=\"r num\">{count}</td></tr>",
        render_dots(lesson, planned.and_then(|p| p.planned_nodes))
    )
}

fn lesson_href(lesson: &LessonSummary) -> String {
    format!("courses/{}/lessons/{}", lesson.course, lesson.filename)
}

/// The cheat sheet compiled from the lesson — the artifact he actually re-reads.
fn reference_href(lesson: &LessonSummary) -> String {
    format!("courses/{}/reference/{}.html", lesson.course, lesson.slug)
}

/// `course` links the chapter header to its directory, where MISSION.md and the rest live.
fn render_section(
    title: &str,
    taught: usize,
    total: usize,
    rows: &str,
    course: Option<&str>,
) -> String {
    let pct = if total > 0 {
        to_fixed_1(taught as f64 / total as f64 * 100.0)
    } else {
        to_fixed_1(0.0)
    };
    let heading = match course {
        Some(course) => format!(
            "<a href=\"courses/{}/\">{}</a>",
            encode_uri(course),
            escape_html(title)
        ),
        None => escape_html(title),
    };
    format!(
        "<div class=\"chapter-h\">{heading}<span class=\"num\">{taught}/{total}</span></div>\
         <div class=\"bar\"><i style=\"width:{pct}%\"></i></div>\
         <div class=\"card\"><table class=\"reg\">{rows}</table></div>"
    )
}

fn render_anki_strip(anki: &AnkiStatus) -> String {
    let stat = |value: i64, label: &str, hot: bool| {
        let hot = if hot { " hot" } else { "" };
        format!("<div class=\"stat{hot}\"><b>{value}</b><span>{label}</span></div>")
    };
    format!(
        "<div class=\"card anki-strip\">{}{}{}<div class=\"spacer\"></div>\
         <div class=\"stat\" style=\"text-align:right\"><b>{}</b><span>card in {} deck</span></div></div>",
        stat(anki.due, "da ripassare", true),
        stat(anki.new_cards, "nuove", false),
        stat(anki.learning, "in apprendimento", false),
        anki.total,
        anki.deck_count
    )
}

pub fn render_homepage(m: &CurriculumMatch, anki: Option<&AnkiStatus>) -> String {
    let sections: String = m
        .chapters
        .iter()
        .map(|chapter| {
            let rows: String = chapter
                .lessons
                .iter()
                .map(|row| render_row(Some(&row.planned), row.lesson.as_ref()))
                .collect();
            render_section(
                &chapter.title,
                chapter.taught,
                chapter.lessons.len(),
                &rows,
                Some(&chapter.slug),
            )
        })
        .collect();

    // A taught lesson whose slug matches no plan entry still has to be reachable.
    let orphans = &m.orphans;
    let orphan_section = if orphans.is_empty() {
        String::new()
    } else {
        let rows: String = orphans
            .iter()
            .map(|lesson| render_row(None, Some(lesson)))
            .collect();
        render_section("Fuori dal piano", orphans.len(), orphans.len(), &rows, None)
    };

    let taught_total = m.taught.len();
    let planned_total: usize = m.chapters.iter().map(|c| c.lessons.len()).sum();
    let node_total: usize = m.taught.iter().map(|l| l.node_count).sum();
    let subtitle = format!(
        "{} {} &middot; {}/{} lezioni fatte &middot; {} {}",
        m.chapters.len(),
        if m.chapters.len() == 1 {
            "capitolo"
        } else {
            "capitoli"
        },
        taught_total,
        planned_total.max(taught_total),
        node_total,
        if node_total == 1 {
            "nodo catturato"
        } else {
            "nodi catturati"
        }
    );

    let stamp = match anki {
        Some(anki) => format!(
            "Dati Anki al {} &mdash; rigenera con <code>npm run home</code>",
            escape_html(&clock::it_it_local(anki.generated_at))
        ),
        None => "Generata senza Anki (<code>--no-anki</code>) &mdash; rigenera con <code>npm run home</code>"
            .to_string(),
    };
    let strip = anki.map(render_anki_strip).unwrap_or_default();
    let board = if sections.is_empty() && orphan_section.is_empty() {
        "<div class=\"card empty\">Nessun capitolo pianificato. Comincia con <code>/plan &lt;argomento&gt;</code>.</div>".to_string()
    } else {
        format!("{sections}{orphan_section}")
    };

    let body = format!(
        "<h1>{APP_NAME}</h1><div class=\"sub\">{subtitle}</div>{strip}{board}{LEGEND}<div class=\"stamp\">{stamp}</div>"
    );
    HOMEPAGE_SHELL.replacen("<!-- BODY -->", &body, 1)
}

const LEGEND: &str =
    "<div class=\"legend\">\
     <span><span class=\"dots\"><i class=\"truth\"></i></span> verità incondizionata</span>\
     <span><span class=\"dots\"><i class=\"axiom\"></i></span> assioma</span>\
     <span><span class=\"dots\"><i class=\"deriv\"></i></span> derivazione</span>\
     <span><span class=\"dots\"><i class=\"pending\"></i></span> nodo previsto</span></div>";

// Verbatim from homepage.ts, with APP_NAME spliced in where TS concatenates it.
const HOMEPAGE_SHELL: &str =
    "<!DOCTYPE html>\n\
     <html lang=\"it\">\n\
     <head>\n\
     <meta charset=\"utf-8\">\n\
     <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
     <title>Proxi</title>\n\
     <style>\n\
     :root{--font-body:Charter,\"Bitstream Charter\",\"Sitka Text\",Cambria,Georgia,serif;\
     --font-ui:-apple-system,BlinkMacSystemFont,\"Segoe UI\",Roboto,Inter,Helvetica,Arial,sans-serif;\
     --font-mono:ui-monospace,\"SF Mono\",Monaco,\"Cascadia Code\",Consolas,monospace;\
     --bg:#fcfbfa;--surface:#fff;--subtle:#f4f3ef;--fg:#1c1917;--fg2:#44403c;--muted:#78716c;\
     --border:#e7e5e0;--link:#0369a1;--ok:#0f766e;--due:#b91c1c;\
     --truth:#b45309;--axiom:#0f766e;--deriv:#4338ca}\n\
     @media(prefers-color-scheme:dark){:root{--bg:#0f172a;--surface:#1e293b;--subtle:#192434;\
     --fg:#f8fafc;--fg2:#cbd5e1;--muted:#94a3b8;--border:#334155;--link:#7dd3fc;--ok:#2dd4bf;--due:#fca5a5;\
     --truth:#fbbf24;--axiom:#2dd4bf;--deriv:#a5b4fc}}\n\
     *{box-sizing:border-box}\n\
     body{margin:0;background:var(--bg);color:var(--fg);font-family:var(--font-ui);font-size:15px;\
     line-height:1.5;padding:2rem 1.25rem 4rem}\n\
     .wrap{max-width:960px;margin:0 auto}\n\
     h1{font-family:var(--font-body);font-size:1.6rem;margin:0 0 .15rem}\n\
     .sub{color:var(--muted);font-size:.85rem;margin-bottom:1.75rem}\n\
     a{color:var(--link);text-decoration:none}a:hover{text-decoration:underline}\n\
     .num{font-family:var(--font-mono);font-size:.8rem;color:var(--muted)}\n\
     .card{background:var(--surface);border:1px solid var(--border);border-radius:8px}\n\
     .empty{padding:1.25rem;color:var(--muted);font-size:.9rem}\n\
     code{font-family:var(--font-mono);font-size:.85em;background:var(--subtle);padding:.1em .3em;border-radius:4px}\n\
     .bar{height:4px;background:var(--subtle);border-radius:2px;overflow:hidden;margin-bottom:.6rem}\n\
     .bar>i{display:block;height:100%;background:var(--ok)}\n\
     .dots{display:inline-flex;gap:2px;align-items:center;vertical-align:middle}\n\
     .dots i{width:6px;height:6px;border-radius:50%;display:block}\n\
     .dots i.truth{background:var(--truth)}.dots i.axiom{background:var(--axiom)}\n\
     .dots i.deriv{background:var(--deriv)}.dots i.pending{background:var(--border)}\n\
     .legend{font-size:.72rem;color:var(--muted);display:flex;gap:1rem;margin-top:2rem;\
     padding-top:.8rem;border-top:1px solid var(--border);flex-wrap:wrap}\n\
     .legend span{display:flex;gap:.3rem;align-items:center}\n\
     .stamp{font-size:.72rem;color:var(--muted);margin-top:.8rem}\n\
     .anki-strip{display:flex;align-items:center;gap:1.5rem;padding:.8rem 1rem;margin-bottom:1.75rem;flex-wrap:wrap}\n\
     .anki-strip .stat{display:flex;flex-direction:column}\n\
     .anki-strip .stat b{font-family:var(--font-mono);font-size:1.3rem;font-weight:600}\n\
     .anki-strip .stat.hot b{color:var(--due)}\n\
     .anki-strip .stat span{font-size:.7rem;text-transform:uppercase;letter-spacing:.04em;color:var(--muted)}\n\
     .anki-strip .spacer{flex:1}\n\
     .chapter-h{font-family:var(--font-body);font-size:1.05rem;margin:1.75rem 0 .4rem;\
     display:flex;align-items:baseline;gap:.6rem}\n\
     .chapter-h .num{margin-left:auto}\n\
     table.reg{width:100%;border-collapse:collapse}\n\
     table.reg td{padding:.45rem .5rem;border-bottom:1px solid var(--border);vertical-align:middle}\n\
     table.reg tr:last-child td{border-bottom:none}\n\
     table.reg td.mark{width:1.6rem;text-align:center;color:var(--ok)}\n\
     table.reg td.r{text-align:right;white-space:nowrap}\n\
     table.reg tr.is-todo td.title{color:var(--muted)}\n\
     a.ref{font-family:var(--font-mono);font-size:.7rem;text-transform:uppercase;letter-spacing:.05em;color:var(--muted);text-decoration:none;border:1px solid var(--muted);border-radius:.25rem;padding:0 .25rem;margin-left:.4rem;opacity:.7}\n\
     a.ref:hover{opacity:1}\n\
     </style>\n\
     </head>\n\
     <body>\n\
     <div class=\"wrap\">\n\
     <!-- BODY -->\n\
     </div>\n\
     </body>\n\
     </html>\n";

/// The explicit inputs of one generation: every path resolves against `base_dir` (the data root).
pub struct GenerateOptions {
    pub base_dir: PathBuf,
    pub courses_dir: Option<String>,
    pub curriculum: Option<String>,
    pub out: Option<String>,
    pub no_anki: bool,
    pub anki_url: Option<String>,
    pub timeout: Duration,
}

#[derive(Debug)]
pub struct GenerateResult {
    pub file_path: PathBuf,
    pub chapter_count: usize,
    pub lesson_count: usize,
    pub orphan_count: usize,
    pub anki_online: bool,
}

pub async fn generate_homepage(
    options: &GenerateOptions,
    warn: &mut dyn Write,
) -> Result<GenerateResult, String> {
    let base_dir = &options.base_dir;
    let out_file = match options.out.as_deref().filter(|out| !out.is_empty()) {
        Some(out) => data_root::resolve_path_from(base_dir, Path::new(out)),
        None => base_dir.join("index.html"),
    };

    let mut anki: Option<AnkiStatus> = None;
    if !options.no_anki {
        let url = options
            .anki_url
            .as_deref()
            .filter(|url| !url.is_empty())
            .unwrap_or(DEFAULT_ANKI_URL);
        // byCourse needs the same baseDir the plan and the scan below use, not the cwd's.
        let status = board_anki_status(
            url,
            options.timeout,
            base_dir,
            &courses_dir_for(base_dir, options.courses_dir.as_deref()),
            &curriculum_file(base_dir, options.curriculum.as_deref()),
            &mut *warn,
        )
        .await
        .map_err(|error| {
            format!(
                "AnkiConnect non raggiungibile ({}). Apri Anki, oppure genera senza i dati delle card con --no-anki.",
                if error.is_empty() { "unknown" } else { error.as_str() }
            )
        })?;
        anki = Some(status);
    }

    let curriculum = read_curriculum(&curriculum_file(
        base_dir,
        options.curriculum.as_deref(),
    ))?;
    let courses_dir = courses_dir_for(base_dir, options.courses_dir.as_deref());
    let matched = match_curriculum(&curriculum, scan_lessons(&courses_dir, warn));

    let html = render_homepage(&matched, anki.as_ref());
    fs::write(&out_file, html.as_bytes()).map_err(|error| error.to_string())?;

    Ok(GenerateResult {
        file_path: out_file,
        chapter_count: matched.chapters.len(),
        lesson_count: matched.taught.len(),
        orphan_count: matched.orphans.len(),
        anki_online: anki.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::*;
    use crate::anki::DEFAULT_TIMEOUT;
    use crate::chapter::{curriculum_file, plan_chapter, Curriculum};

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "proxi-homepage-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn write(file: &Path, body: &str) {
        fs::create_dir_all(file.parent().expect("parent")).expect("create parent");
        fs::write(file, body).expect("write file");
    }

    fn lesson_doc(
        lesson_number: i64,
        topic: &str,
        title: &str,
        slug: &str,
        tiers: &[&str],
    ) -> Value {
        json!({
            "metadata": {
                "lessonNumber": lesson_number,
                "topic": topic,
                "title": title,
                "slug": slug,
                "dateCreated": "2026-09-01T00:00:00.000Z",
                "dateModified": "2026-09-01T00:00:00.000Z",
                "version": "1.0.0"
            },
            "nodes": tiers.iter().enumerate().map(|(i, tier)| json!({
                "nodeId": format!("n{i}"),
                "title": format!("Nodo {i}"),
                "tier": tier,
                "content": "..."
            })).collect::<Vec<_>>()
        })
    }

    /// Writes a lesson artifact the way the retrofit and `lesson append` both do: island last.
    fn write_lesson(
        base_dir: &Path,
        course: &str,
        lesson_number: i64,
        slug: &str,
        title: &str,
        tiers: &[&str],
    ) {
        let doc = lesson_doc(lesson_number, course, title, slug, tiers);
        write_island(
            &base_dir
                .join("courses")
                .join(course)
                .join("lessons")
                .join(format!("{:04}-{slug}.html", lesson_number)),
            title,
            &doc,
        );
    }

    fn write_island(file: &Path, title: &str, doc: &Value) {
        write(
            file,
            &format!(
                "<html><body><h1>{title}</h1>\n<script type=\"application/json\" id=\"lesson-data\">\n{}\n</script>\n</body></html>",
                serde_json::to_string_pretty(doc).expect("doc json")
            ),
        );
    }

    fn chapter(title: &str, slug: &str, lessons: Vec<Value>) -> serde_json::Map<String, Value> {
        json!({ "title": title, "slug": slug, "lessons": lessons })
            .as_object()
            .expect("chapter object")
            .clone()
    }

    fn curriculum(chapters: Vec<serde_json::Map<String, Value>>) -> Curriculum {
        Curriculum { chapters }
    }

    fn planned(title: &str, slug: &str, planned_nodes: Option<f64>) -> Value {
        let mut lesson = json!({ "title": title, "slug": slug });
        if let Some(nodes) = planned_nodes {
            lesson["plannedNodes"] = json!(nodes);
        }
        lesson
    }

    fn summary(
        course: &str,
        lesson_number: i64,
        slug: &str,
        title: &str,
        filename: &str,
        node_count: usize,
        tiers: &[&str],
    ) -> LessonSummary {
        LessonSummary {
            course: course.to_string(),
            lesson_number,
            slug: slug.to_string(),
            title: title.to_string(),
            filename: filename.to_string(),
            node_count,
            tiers: tiers.iter().map(|tier| tier.to_string()).collect(),
            has_reference: false,
        }
    }

    fn plain_summary(course: &str, lesson_number: i64, slug: &str, title: &str) -> LessonSummary {
        summary(
            course,
            lesson_number,
            slug,
            title,
            &format!("{:04}-{slug}.html", lesson_number),
            1,
            &["axiom"],
        )
    }

    fn lessons_json(raw: &str) -> Vec<Value> {
        serde_json::from_str(raw).expect("lessons json")
    }

    #[test]
    fn walks_every_course_keeping_per_course_numbering_and_tiers() {
        let t = temp_dir("scan");
        write_lesson(
            &t.0,
            "docker",
            1,
            "image-e-container",
            "Image e container",
            &["unconditional-truth", "derivation"],
        );
        write_lesson(&t.0, "app-cache", 1, "cache-in-memory", "Cache in memory", &[]);

        let mut warn = Vec::new();
        let lessons = scan_lessons(&courses_dir_for(&t.0, None), &mut warn);
        assert_eq!(lessons.len(), 2);
        assert!(warn.is_empty());

        let docker = lessons.iter().find(|l| l.course == "docker").expect("docker");
        assert_eq!(docker.lesson_number, 1);
        assert_eq!(docker.slug, "image-e-container");
        assert_eq!(docker.node_count, 2);
        assert_eq!(docker.tiers, ["unconditional-truth", "derivation"]);

        let cache = lessons.iter().find(|l| l.course == "app-cache").expect("cache");
        assert_eq!(cache.lesson_number, 1, "numbering restarts per course");
        assert_eq!(cache.node_count, 0, "a hand-authored lesson has no captured nodes");
    }

    #[test]
    fn skips_an_html_file_with_no_data_island() {
        let t = temp_dir("no-island");
        write(
            &t.0.join("courses").join("docker").join("lessons").join("0001-prosa.html"),
            "<html><body><h1>Prosa</h1></body></html>",
        );

        let mut warn = Vec::new();
        assert!(scan_lessons(&courses_dir_for(&t.0, None), &mut warn).is_empty());
        assert!(warn.is_empty());
    }

    #[test]
    fn reports_a_corrupt_island_and_keeps_scanning() {
        let t = temp_dir("corrupt");
        write_lesson(&t.0, "docker", 1, "image-e-container", "Image e container", &["axiom"]);
        write(
            &t.0.join("courses").join("docker").join("lessons").join("0002-rotta.html"),
            "<html><script type=\"application/json\" id=\"lesson-data\">{ \"metadata\": </script></html>",
        );

        let mut warn = Vec::new();
        let lessons = scan_lessons(&courses_dir_for(&t.0, None), &mut warn);
        assert_eq!(lessons.len(), 1, "the healthy lesson still shows up");
        assert_eq!(lessons[0].slug, "image-e-container");
        let warn = String::from_utf8(warn).expect("warn text");
        assert!(
            warn.starts_with("Ignorata docker/0002-rotta.html: Malformed #lesson-data island: "),
            "{warn}"
        );
    }

    #[test]
    fn keeps_a_slug_shadowed_lesson_as_an_orphan() {
        let t = temp_dir("shadow");
        write_lesson(&t.0, "docker", 1, "compose", "Compose, prima versione", &[]);
        write_lesson(&t.0, "docker", 2, "compose", "Compose, seconda versione", &[]);
        let file = curriculum_file(&t.0, None);
        let planned_curriculum = plan_chapter(
            &file,
            "Docker",
            None,
            &lessons_json(r#"[{"title":"Compose","slug":"compose"}]"#),
        )
        .expect("plans");

        let mut warn = Vec::new();
        let matched = match_curriculum(
            &planned_curriculum,
            scan_lessons(&courses_dir_for(&t.0, None), &mut warn),
        );
        assert_eq!(matched.chapters[0].taught, 1);
        // The one the plan did not claim must still surface somewhere, not vanish from the board.
        assert_eq!(matched.orphans.len(), 1);
        assert_eq!(matched.orphans[0].lesson_number, 2);
    }

    #[test]
    fn returns_an_empty_list_when_no_course_exists() {
        let t = temp_dir("empty");
        let mut warn = Vec::new();
        assert!(scan_lessons(&courses_dir_for(&t.0, None), &mut warn).is_empty());
    }

    #[test]
    fn links_a_taught_lesson_and_never_a_plan_entry() {
        let matched = match_curriculum(
            &curriculum(vec![chapter(
                "Docker",
                "docker",
                vec![
                    planned("Image e container", "image-e-container", None),
                    planned("Healthcheck", "healthcheck", Some(4.0)),
                ],
            )]),
            vec![plain_summary(
                "docker",
                1,
                "image-e-container",
                "Image e container",
            )],
        );
        let html = render_homepage(&matched, None);

        assert!(html.contains("href=\"courses/docker/lessons/0001-image-e-container.html\""));
        assert!(html.contains("1/2"));
        assert!(html.contains("Healthcheck"));
        assert!(html.contains("<td class=\"title\">Healthcheck</td>"));
        assert!(html.contains("4 nodi previsti"));
        assert!(html.contains("href=\"courses/docker/\""), "chapter links to its course directory");
    }

    #[test]
    fn does_not_let_one_course_claim_anothers_plan_entry() {
        let matched = match_curriculum(
            &curriculum(vec![
                chapter("Docker", "docker", vec![planned("Basi", "basi", None)]),
                chapter("Nginx", "nginx", vec![planned("Basi", "basi", None)]),
            ]),
            vec![summary("docker", 1, "basi", "Basi", "0001-basi.html", 1, &["axiom"])],
        );

        assert_eq!(matched.chapters[0].taught, 1);
        assert_eq!(matched.chapters[1].taught, 0, "nginx/basi is still to do");
        assert!(matched.orphans.is_empty());
    }

    #[test]
    fn puts_an_unplanned_lesson_under_its_own_section() {
        let matched = match_curriculum(
            &Curriculum::default(),
            vec![plain_summary("docker", 1, "fuori-piano", "Fuori piano")],
        );
        assert_eq!(matched.orphans.len(), 1);
        let html = render_homepage(&matched, None);
        assert!(html.contains("Fuori dal piano"));
        assert!(html.contains("href=\"courses/docker/lessons/0001-fuori-piano.html\""));
    }

    #[test]
    fn titles_the_page_proxi() {
        let matched = match_curriculum(
            &Curriculum::default(),
            vec![plain_summary("docker", 1, "fuori-piano", "Fuori piano")],
        );
        let html = render_homepage(&matched, None);
        assert!(html.contains("<title>Proxi</title>"));
        assert!(html.contains("<h1>Proxi</h1>"));
    }

    #[test]
    fn reads_a_hand_authored_lesson_as_prose() {
        let matched = match_curriculum(
            &Curriculum::default(),
            vec![summary("docker", 1, "prosa", "Prosa", "0001-prosa.html", 0, &[])],
        );
        let html = render_homepage(&matched, None);
        assert!(html.contains(">prosa<"));
        assert!(!html.contains(">0 nodi<"), "the row reads prosa, not a node count of zero");
    }

    #[test]
    fn renders_anki_counts_when_present_and_omits_the_strip_otherwise() {
        let generated_at = UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_000);
        let status = AnkiStatus {
            due: 23,
            new_cards: 12,
            learning: 5,
            total: 148,
            deck_count: 3,
            generated_at,
            by_course: Vec::new(),
        };
        let matched = match_curriculum(&Curriculum::default(), Vec::new());
        let html = render_homepage(&matched, Some(&status));
        assert!(html.contains("da ripassare"));
        assert!(html.contains(">23<"));
        assert!(html.contains(&format!("Dati Anki al {}", clock::it_it_local(generated_at))));
        assert!(!render_homepage(&matched, None).contains("da ripassare"));
    }

    #[test]
    fn escapes_titles_from_the_plan_and_lesson_metadata() {
        let matched = match_curriculum(
            &curriculum(vec![chapter(
                "<script>alert(1)</script>",
                "x",
                vec![planned("a & b", "a-b", None)],
            )]),
            vec![summary("x", 1, "a-b", "a & b", "0001-a-b.html", 1, &["axiom"])],
        );
        let html = render_homepage(&matched, None);
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("a &amp; b"));
    }

    #[test]
    fn encode_uri_keeps_js_reserved_chars() {
        assert_eq!(
            encode_uri("courses/a b/è#?.html"),
            "courses/a%20b/%C3%A8#?.html"
        );
    }

    #[test]
    fn render_row_encodes_spaces_and_accents_in_hrefs() {
        let matched = match_curriculum(
            &Curriculum::default(),
            vec![summary(
                "docker",
                2,
                "con-spazio",
                "Con spazio è",
                "0002-con spazio è.html",
                1,
                &["axiom"],
            )],
        );
        let html = render_homepage(&matched, None);
        assert!(html.contains("href=\"courses/docker/lessons/0002-con%20spazio%20%C3%A8.html\""));
    }

    #[test]
    fn scan_lessons_orders_by_course_then_lesson_number() {
        let t = temp_dir("order");
        // Created in the reverse of the expected order on purpose.
        write_lesson(&t.0, "beta", 2, "a-second", "Second", &[]);
        write_lesson(&t.0, "beta", 1, "b-first", "First", &[]);
        write_lesson(&t.0, "alpha", 2, "x", "X", &[]);
        write_lesson(&t.0, "alpha", 1, "y", "Y", &[]);

        let mut warn = Vec::new();
        let lessons = scan_lessons(&courses_dir_for(&t.0, None), &mut warn);
        let order: Vec<(String, i64, String)> = lessons
            .into_iter()
            .map(|lesson| (lesson.course, lesson.lesson_number, lesson.filename))
            .collect();
        assert_eq!(
            order,
            [
                ("alpha".to_string(), 1, "0001-y.html".to_string()),
                ("alpha".to_string(), 2, "0002-x.html".to_string()),
                ("beta".to_string(), 1, "0001-b-first.html".to_string()),
                ("beta".to_string(), 2, "0002-a-second.html".to_string()),
            ]
        );
    }

    #[test]
    fn orphan_sections_order_by_course_then_number() {
        let t = temp_dir("orphans");
        write_lesson(&t.0, "docker", 4, "d", "D", &["axiom"]);
        write_lesson(&t.0, "docker", 2, "b", "B", &["axiom"]);
        write_lesson(&t.0, "probabilita", 2, "c", "C", &["axiom"]);

        let mut warn = Vec::new();
        let lessons = scan_lessons(&courses_dir_for(&t.0, None), &mut warn);
        let matched = match_curriculum(&Curriculum::default(), lessons);
        assert!(matched.chapters.is_empty());
        let html = render_homepage(&matched, None);

        let block = html.find("Fuori dal piano").expect("orphan section");
        let positions: Vec<usize> = [
            "courses/docker/lessons/0002-b.html",
            "courses/docker/lessons/0004-d.html",
            "courses/probabilita/lessons/0002-c.html",
        ]
        .iter()
        .map(|href| html.find(href).unwrap_or_else(|| panic!("missing {href}")))
        .collect();
        assert!(positions.iter().all(|&position| position > block));
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "sorted by course then number, got {positions:?}"
        );
    }

    #[test]
    fn to_fixed_1_matches_js() {
        assert_eq!(to_fixed_1(6.25), "6.3");
        assert_eq!(to_fixed_1(1.0 / 16.0 * 100.0), "6.3");
        assert_eq!(to_fixed_1(200.0 / 3.0), "66.7");
        assert_eq!(to_fixed_1(0.0), "0.0");
        assert_eq!(to_fixed_1(100.0), "100.0");
        assert_eq!(to_fixed_1(0.05), "0.1");
    }

    #[tokio::test]
    async fn generate_writes_index_without_anki() {
        let t = temp_dir("generate");
        write_lesson(&t.0, "docker", 1, "image-e-container", "Image e container", &["axiom"]);
        let file = curriculum_file(&t.0, None);
        plan_chapter(
            &file,
            "Docker",
            None,
            &lessons_json(r#"[{"title":"Image e container"}]"#),
        )
        .expect("plans");

        let mut warn = Vec::new();
        let result = generate_homepage(
            &GenerateOptions {
                base_dir: t.0.clone(),
                courses_dir: None,
                curriculum: None,
                out: None,
                no_anki: true,
                anki_url: None,
                timeout: DEFAULT_TIMEOUT,
            },
            &mut warn,
        )
        .await
        .expect("generates");

        assert_eq!(result.file_path, t.0.join("index.html"));
        assert!(!result.anki_online);
        assert_eq!(result.chapter_count, 1);
        assert_eq!(result.lesson_count, 1);
        assert_eq!(result.orphan_count, 0);
        let html = fs::read_to_string(&result.file_path).expect("index.html");
        assert!(html.contains("href=\"courses/docker/lessons/0001-image-e-container.html\""));
        assert!(!html.contains('\r'), "only \\n line ends");
        assert!(html.ends_with('\n'));
        assert!(warn.is_empty());
    }

    #[tokio::test]
    async fn generate_refuses_when_anki_is_unreachable() {
        let t = temp_dir("unreachable");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind closed port");
        let port = listener.local_addr().expect("address").port();
        drop(listener);

        let mut warn = Vec::new();
        let error = generate_homepage(
            &GenerateOptions {
                base_dir: t.0.clone(),
                courses_dir: None,
                curriculum: None,
                out: None,
                no_anki: false,
                anki_url: Some(format!("http://127.0.0.1:{port}")),
                timeout: DEFAULT_TIMEOUT,
            },
            &mut warn,
        )
        .await
        .expect_err("refuses");

        assert!(error.contains("--no-anki"), "{error}");
        assert!(error.contains("fetch failed"), "{error}");
        assert!(!t.0.join("index.html").exists());
    }

    #[tokio::test]
    async fn generate_reads_the_plan_from_the_given_curriculum_and_courses_dir() {        let t = temp_dir("generate-options");
        write_lesson(&t.0, "rust", 1, "ownership", "Ownership", &["axiom"]);
        write(
            &t.0.join("plan.json"),
            "{\n  \"chapters\": []\n}\n",
        );
        // The plan lives outside the default location; a default read would see no chapters.
        let plan = curriculum_file(&t.0, Some("plan.json"));
        plan_chapter(
            &plan,
            "Rust",
            None,
            &lessons_json(r#"[{"title":"Ownership"}]"#),
        )
        .expect("plans");

        let mut warn = Vec::new();
        let result = generate_homepage(
            &GenerateOptions {
                base_dir: t.0.clone(),
                courses_dir: Some("courses".to_string()),
                curriculum: Some("plan.json".to_string()),
                out: Some("board.html".to_string()),
                no_anki: true,
                anki_url: None,
                timeout: DEFAULT_TIMEOUT,
            },
            &mut warn,
        )
        .await
        .expect("generates");

        assert_eq!(result.file_path, t.0.join("board.html"));
        assert_eq!(result.chapter_count, 1);
        assert_eq!(result.lesson_count, 1);
        assert!(!t.0.join("index.html").exists());
        let html = fs::read_to_string(&result.file_path).expect("board.html");
        assert!(html.contains("href=\"courses/rust/\""));
        assert!(html.contains("0001-ownership.html"));
    }
}
