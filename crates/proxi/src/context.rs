//! The system prompt and opening message built from settings and course data (port of `app/server/context.ts`).

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use crate::courses::{self, Chapter, Match};
use crate::data_root::resolve_path;
use crate::graph::store::GraphStore;
use crate::js;
use crate::knowledge::{self, MapNode};
use crate::lesson::{self, Lesson};

const OPEN_PICKED: &str = "Opening: he picked these on the map. Start at once with a problem on them; do not ask him to choose a topic.";

const OPEN_TOOLS: &str = "Opening: nothing picked on the map. Before your first message call mcp__app__browse_concepts on this course and open the concepts that look fragile or mancato with mcp__app__open_concept. Then ask him, in one short question, whether he wants exercises on a topic of his choice or on those weak concepts, naming up to three by name; if none is fragile or mancato, ask which topic he wants or whether you should choose. Wait for his answer before posing a problem.";

const REFERENCES_PREFACE: &str = "# Reference documents\n\nWritten for other runtimes. Where they mention `npm` commands, subagents, rendering or render-check tools, `ask_user_question`, or asking the Anki opt-in, the app tools and the `# This session` block override them (`mcp__app__ask`, WebSearch/WebFetch, the write/finish tools; the opt-in is already decided by the UI). The interactive quiz card is shuffled, but the mirror quiz in a node artifact is not: vary the position of the correct option. Everything else applies.";

pub struct PromptInput<'a> {
    pub config: &'a Map<String, Value>,
    pub code_root: &'a Path,
    pub repo_root: &'a Path,
    pub data_root: Option<&'a Path>,
    pub lesson_path: Option<&'a str>,
    pub today: &'a str,
}

fn read_if(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Some(js::js_trim(&text).to_string())
}

/// Wraps a file as `<file path="...">`, or "" when missing/empty.
fn file_block(root: &Path, rel: &str) -> String {
    match read_if(&root.join(rel)) {
        Some(text) if !text.is_empty() => format!("<file path=\"{rel}\">\n{text}\n</file>"),
        _ => String::new(),
    }
}

fn is_record_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 8
        && bytes[..4].iter().all(|b| b.is_ascii_digit())
        && bytes[4] == b'-'
        && name.ends_with(".md")
}

fn last_records(data_root: &Path, course: &str, n: usize) -> Vec<String> {
    let dir = data_root
        .join("courses")
        .join(course)
        .join("learning-records");
    if !dir.exists() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| is_record_name(name))
        .collect();
    files.sort();
    let skip = files.len().saturating_sub(n);
    files
        .into_iter()
        .skip(skip)
        .map(|file| format!("courses/{course}/learning-records/{file}"))
        .collect()
}

fn line_of(lesson: Option<&Lesson>, planned: &Value) -> String {
    match lesson {
        Some(lesson) => {
            let number = js::js_string(lesson.summary.lesson_number.as_ref());
            let suffix = match lesson.summary.node_count {
                0 => " (prosa)".to_string(),
                1 => " (1 nodo)".to_string(),
                count => format!(" ({count} nodi)"),
            };
            format!(
                "  [x] {number:0>4}  {}{suffix}",
                js::display(lesson.summary.title.as_ref())
            )
        }
        None => {
            let suffix = if js::truthy(planned.get("plannedNodes")) {
                format!(
                    " ({} nodi previsti)",
                    js::display(planned.get("plannedNodes"))
                )
            } else {
                String::new()
            };
            format!("  [ ] ----  {}{suffix}", js::display(planned.get("title")))
        }
    }
}

/// `formatCurriculum` (extensions/chapter.ts:188-218) over the selected chapters and orphans.
fn format_curriculum(
    chapters: &[Chapter],
    lessons: &[Lesson],
    matched: &Match,
    selected: &[usize],
    orphans: &[usize],
) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for &chapter_idx in selected {
        let Some((_, entries)) = matched.chapters.iter().find(|(idx, _)| *idx == chapter_idx)
        else {
            continue;
        };
        let chapter = &chapters[chapter_idx];
        let taught = entries
            .iter()
            .filter(|(_, lesson_idx)| lesson_idx.is_some())
            .count();
        let lines: Vec<String> = entries
            .iter()
            .map(|(planned, lesson_idx)| line_of(lesson_idx.map(|idx| &lessons[idx]), planned))
            .collect();
        blocks.push(format!(
            "{} (courses/{}) — {}/{}\n{}",
            js::display(chapter.title.as_ref()),
            js::js_string(chapter.slug.as_ref()),
            taught,
            entries.len(),
            lines.join("\n")
        ));
    }
    if !orphans.is_empty() {
        let lines: Vec<String> = orphans
            .iter()
            .map(|&idx| {
                let lesson = &lessons[idx];
                format!(
                    "{}  [courses/{}]",
                    line_of(Some(lesson), &Value::Null),
                    lesson.summary.course
                )
            })
            .collect();
        blocks.push(format!("Fuori dal piano\n{}", lines.join("\n")));
    }
    if blocks.is_empty() {
        "Nessun capitolo pianificato.".to_string()
    } else {
        blocks.join("\n\n")
    }
}

fn chapter_plan(data_root: &Path, course: &str) -> String {
    let lessons = lesson::list_lessons(data_root);
    let Ok(chapters) = courses::read_curriculum(data_root) else {
        return String::new();
    };
    let matched = courses::match_curriculum(&chapters, &lessons);
    let selected: Vec<usize> = matched
        .chapters
        .iter()
        .filter(|(idx, _)| chapters[*idx].slug.as_ref().and_then(Value::as_str) == Some(course))
        .map(|(idx, _)| *idx)
        .collect();
    let orphans: Vec<usize> = matched
        .orphans
        .iter()
        .copied()
        .filter(|&idx| lessons[idx].summary.course == course)
        .collect();
    if selected.is_empty() && orphans.is_empty() {
        return String::new();
    }
    format_curriculum(&chapters, &lessons, &matched, &selected, &orphans)
}

fn exercise_names(data_root: &Path, course: &str) -> Vec<String> {
    let dir = data_root.join("courses").join(course).join("exercises");
    if !dir.join("package.json").exists() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| {
            let bytes = name.as_bytes();
            (bytes.len() >= 3
                && bytes[0].is_ascii_digit()
                && bytes[1].is_ascii_digit()
                && bytes[2] == b'-')
                || name.starts_with("zz-")
        })
        .collect();
    names.sort();
    names
}

fn course_context(data_root: &Path, config: &Map<String, Value>) -> String {
    let course = js::js_string(config.get("course"));
    let mut parts: Vec<String> = Vec::new();
    for rel in [
        format!("courses/{course}/MISSION.md"),
        format!("courses/{course}/NOTES.md"),
        format!("courses/{course}/GLOSSARY.md"),
    ] {
        let block = file_block(data_root, &rel);
        if !block.is_empty() {
            parts.push(block);
        }
    }
    for rel in last_records(data_root, &course, 3) {
        let block = file_block(data_root, &rel);
        if !block.is_empty() {
            parts.push(block);
        }
    }
    let plan = chapter_plan(data_root, &course);
    if !plan.is_empty() {
        parts.push(format!("<chapter-plan>\n{plan}\n</chapter-plan>"));
    }
    let exercises = exercise_names(data_root, &course);
    if !exercises.is_empty() {
        parts.push(format!(
            "<exercises course=\"{course}\">\n{}\n</exercises>",
            exercises.join("\n")
        ));
    }
    format!(
        "# Course context\n\nCourse directory: courses/{course}\n\n{}",
        if parts.is_empty() {
            "(no course files yet)".to_string()
        } else {
            parts.join("\n\n")
        }
    )
}

/// Shown only when the data folder is not the code repo: tells the teacher where the course files are.
fn data_folder_block(data_root: &Path) -> String {
    let root = data_root.display();
    [
        "# Data folder".to_string(),
        String::new(),
        format!(
            "Course files live in the data folder `{root}`, not in the current directory. Every `courses/...`, `graph/...` or `curriculum.json` path in these instructions is relative to it: pass absolute paths (`{root}/courses/...`) to Read, Glob and Grep."
        ),
    ]
    .join("\n")
}

fn lesson_block(data_root: &Path, lesson_path: &str) -> String {
    let lesson = lesson::read_lesson(data_root, lesson_path).ok().flatten();
    let Some(lesson) = lesson else {
        return format!("Lesson artifact: {lesson_path} (unreadable)");
    };
    let metadata = lesson.doc.get("metadata").unwrap_or(&Value::Null);
    let ids = match metadata.get("initialDag").and_then(Value::as_str) {
        Some(dag) if !dag.is_empty() => lesson::parse_dag_ids(dag),
        _ => Vec::new(),
    };
    let nodes = lesson
        .doc
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let node_lines: Vec<String> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            format!(
                "- N{}: nodeId \"{}\", \"{}\" ({})",
                index + 1,
                js::display(node.get("nodeId")),
                js::display(node.get("title")),
                js::display(node.get("tier"))
            )
        })
        .collect();
    let next = ids
        .get(nodes.len())
        .cloned()
        .unwrap_or_else(|| format!("N{}", nodes.len() + 1));
    let done = lesson.dag_nodes > 0 && lesson.pending_nodes == 0;
    let captured = format!(
        "Captured nodes ({}{}):\n{}",
        nodes.len(),
        if ids.is_empty() {
            String::new()
        } else {
            format!(" of {} planned", ids.len())
        },
        if node_lines.is_empty() {
            "(none)".to_string()
        } else {
            node_lines.join("\n")
        }
    );
    let nullish = |key: &str| match metadata.get(key) {
        Some(value) if !value.is_null() => js::display(Some(value)),
        _ => "(none)".to_string(),
    };
    [
        format!(
            "Lesson artifact: {lesson_path} (lesson number {})",
            js::display(metadata.get("lessonNumber"))
        ),
        format!("Topic: {}", js::display(metadata.get("topic"))),
        format!("Title: {}", js::display(metadata.get("title"))),
        format!("Goal: {}", nullish("targetGoal")),
        format!("Plan DAG (initialDag):\n{}", nullish("initialDag")),
        captured,
        if done {
            "All planned nodes are captured: go to Phase 4.".to_string()
        } else {
            format!("First missing plan node: {next}. Resume there.")
        },
    ]
    .join("\n")
}

/// Propositions picked on the map: name, statement and the lessons they come from.
fn propositions_block(data_root: &Path, ids: &[String]) -> String {
    let snapshot = GraphStore::new(data_root.to_path_buf())
        .read_snapshot()
        .to_json();
    let sessions = snapshot.get("sessions").and_then(Value::as_object);
    let nodes = snapshot.get("nodes").and_then(Value::as_array);
    let lines: Vec<String> = ids
        .iter()
        .map(|id| {
            let node = nodes.and_then(|nodes| {
                nodes
                    .iter()
                    .find(|node| node.get("id").and_then(Value::as_str) == Some(id.as_str()))
            });
            let Some(node) = node else {
                return format!("- {id} (not found in the map)");
            };
            let mut from: Vec<String> = Vec::new();
            if let Some(provenance) = node.get("provenance").and_then(Value::as_array) {
                for point in provenance {
                    let chat = point.get("chatId");
                    let key = js::display(chat);
                    let label = sessions
                        .and_then(|sessions| sessions.get(&key))
                        .filter(|value| !value.is_null())
                        .map(|value| js::display(Some(value)))
                        .unwrap_or(key);
                    if !from.contains(&label) {
                        from.push(label);
                    }
                }
            }
            let rooms = from.join("; ");
            let base = format!(
                "- {} | {}",
                js::display(node.get("name")),
                js::display(node.get("statement"))
            );
            if rooms.is_empty() {
                base
            } else {
                format!("{base} | lessons: {rooms}")
            }
        })
        .collect();
    format!(
        "Propositions selected on the map to practise (true statements; build the practice around them):\n{}",
        lines.join("\n")
    )
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items.iter().map(|item| js::display(Some(item))).collect(),
        _ => Vec::new(),
    }
}

fn practice_block(data_root: &Path, config: &Map<String, Value>) -> String {
    let propositions = string_list(config.get("practicePropositions"));
    if !propositions.is_empty() {
        return format!(
            "{}\n\n{OPEN_PICKED}",
            propositions_block(data_root, &propositions)
        );
    }
    let map_nodes: Vec<MapNode> = match knowledge::build_knowledge_map(data_root) {
        Ok(map) => map.nodes,
        Err(_) => Vec::new(),
    };
    let course_name = js::js_string(config.get("course"));
    let course: Vec<&MapNode> = map_nodes
        .iter()
        .filter(|node| {
            node.json.get("course").and_then(Value::as_str) == Some(course_name.as_str())
        })
        .collect();
    let keys = string_list(config.get("practiceNodes"));
    let mut selected: Vec<&MapNode> = Vec::new();
    if !keys.is_empty() {
        for key in &keys {
            let found = course
                .iter()
                .copied()
                .find(|node| node.json.get("key").and_then(Value::as_str) == Some(key.as_str()))
                .or_else(|| {
                    map_nodes.iter().find(|node| {
                        node.json.get("key").and_then(Value::as_str) == Some(key.as_str())
                    })
                });
            if let Some(node) = found {
                selected.push(node);
            }
        }
    }
    if !keys.is_empty() && selected.is_empty() {
        let lines: Vec<String> = keys.iter().map(|key| format!("- {key}")).collect();
        return format!(
            "Selected nodes (keys only, not found in the map):\n{}\n\n{OPEN_PICKED}",
            lines.join("\n")
        );
    }
    let picked = !selected.is_empty();
    // No pick: the bounded lesson-node list gives the problem tool valid keys; the tutor picks from it.
    let list: Vec<&MapNode> = if picked {
        selected
    } else {
        course.iter().copied().take(80).collect()
    };
    let head = if picked {
        "Selected nodes to practise"
    } else {
        "Course nodes (pass the key as problem.nodeKeys)"
    };
    let lines: Vec<String> = list
        .iter()
        .map(|node| {
            format!(
                "- key \"{}\" | {} | status {} | lesson {} ({})",
                js::display(node.json.get("key")),
                js::display(node.json.get("title")),
                js::display(node.json.get("status")),
                js::display(node.json.get("lessonTitle")),
                js::display(node.json.get("lessonPath"))
            )
        })
        .collect();
    let base = format!(
        "{head}:\n{}",
        if lines.is_empty() {
            "(the map is empty)".to_string()
        } else {
            lines.join("\n")
        }
    );
    format!(
        "{base}\n\n{}",
        if picked { OPEN_PICKED } else { OPEN_TOOLS }
    )
}

fn session_block(input: &PromptInput) -> String {
    let config = input.config;
    let data = input.data_root.unwrap_or(input.repo_root);
    let kind = config.get("kind").and_then(Value::as_str).unwrap_or("");
    let mut lines: Vec<String> = vec![
        "# This session".to_string(),
        String::new(),
        format!("Kind: {}", js::display(config.get("kind"))),
        format!("Course: {}", js::display(config.get("course"))),
        format!("Today: {}", input.today),
        format!("Model: {}", js::display(config.get("model"))),
    ];
    if kind == "teach" {
        let target = config.get("target").and_then(Value::as_object);
        let mode = target
            .and_then(|target| target.get("mode"))
            .and_then(Value::as_str);
        match mode {
            Some("planned") => lines.push(format!(
                "Target: planned lesson. Title (use it as is, character for character, never reword): \"{}\"",
                js::display(target.and_then(|target| target.get("title")))
            )),
            Some("existing") => lines.push("Target: existing lesson, resume it.".to_string()),
            _ => lines.push(
                "Target: free lesson outside the plan; propose the title in propose_plan.".to_string(),
            ),
        }
        lines.push(
            if js::truthy(config.get("ankiOptIn")) {
                "Anki opt-in: Sì (already asked by the UI; cards are authored and synced)."
            } else {
                "Anki opt-in: No (already asked by the UI; never author ankiParams, never mention cards)."
            }
            .to_string(),
        );
        let lesson_path = input.lesson_path.map(str::to_string).or_else(|| {
            if mode == Some("existing") {
                target
                    .and_then(|target| target.get("lessonPath"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            } else {
                None
            }
        });
        if let Some(lesson_path) = lesson_path.filter(|path| !path.is_empty()) {
            lines.push(String::new());
            lines.push(lesson_block(data, &lesson_path));
        }
    } else if kind == "plan" {
        let chapter_title = match config.get("chapterTitle") {
            Some(value) if !value.is_null() => js::display(Some(value)),
            _ => "(not given)".to_string(),
        };
        lines.push(format!("Chapter title: {chapter_title}"));
        let course = js::js_string(config.get("course"));
        let dir = data.join("courses").join(&course);
        if dir.exists() {
            lines.push(format!(
                "Course directory courses/{course} exists{}.",
                if dir.join("MISSION.md").exists() {
                    " and has a MISSION.md: do not rewrite it"
                } else {
                    ""
                }
            ));
        } else {
            lines.push(format!(
                "Course directory courses/{course} is new: provide a full mission."
            ));
        }
    } else {
        lines.push(String::new());
        lines.push(practice_block(data, config));
    }
    lines.push(String::new());
    lines.push(crate::courseindex::course_index_block(data));
    if let Some(note) = config.get("note").and_then(Value::as_str) {
        let note = js::js_trim(note);
        if !note.is_empty() {
            lines.push(String::new());
            lines.push(format!("Learner's note: {note}"));
        }
    }
    lines.join("\n")
}

pub fn build_system_prompt(input: &PromptInput) -> String {
    let config = input.config;
    let data = input.data_root.unwrap_or(input.repo_root);
    let kind = js::js_string(config.get("kind"));
    let prompts = input.code_root.join("app").join("prompts");
    let mut parts: Vec<String> = vec![
        read_if(&prompts.join("base.md")).unwrap_or_default(),
        read_if(&prompts.join(format!("{kind}.md"))).unwrap_or_default(),
    ];
    let mut refs: Vec<String> = Vec::new();
    if kind != "practice" {
        refs.push(file_block(input.repo_root, "docs/agents/pedagogy.md"));
    }
    if kind == "teach" && js::truthy(config.get("ankiOptIn")) {
        refs.push(file_block(input.repo_root, "docs/agents/anki-cards.md"));
    }
    let ref_text = refs
        .into_iter()
        .filter(|block| !block.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !ref_text.is_empty() {
        parts.push(format!("{REFERENCES_PREFACE}\n\n{ref_text}"));
    }
    let separate = resolve_path(data) != resolve_path(input.repo_root);
    let folder = if separate && input.data_root.is_some() {
        data_folder_block(data)
    } else {
        String::new()
    };
    parts.push(
        [folder, course_context(data, config)]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
    );
    parts.push(session_block(input));
    let mut prompt = parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    prompt.push('\n');
    prompt
}

pub fn opening_message(config: &Map<String, Value>, lesson_title: Option<&str>) -> String {
    let note = config
        .get("note")
        .and_then(Value::as_str)
        .map(js::js_trim)
        .unwrap_or("");
    let kind = config.get("kind").and_then(Value::as_str).unwrap_or("");
    let head = if kind == "teach" {
        let target = config.get("target").and_then(Value::as_object);
        match target
            .and_then(|target| target.get("mode"))
            .and_then(Value::as_str)
        {
            Some("planned") => format!(
                "Iniziamo la lezione \"{}\".",
                js::display(target.and_then(|target| target.get("title")))
            ),
            Some("existing") => {
                let title = lesson_title
                    .filter(|title| !title.is_empty())
                    .map(|title| format!(" \"{title}\""))
                    .unwrap_or_default();
                format!("Riprendiamo la lezione{title} da dove l'abbiamo lasciata.")
            }
            _ => format!(
                "Iniziamo una lezione libera sul corso {}.",
                js::display(config.get("course"))
            ),
        }
    } else if kind == "plan" {
        let title = match config.get("chapterTitle") {
            Some(value) if !value.is_null() => js::display(Some(value)),
            _ => js::display(config.get("course")),
        };
        format!("Pianifichiamo il corso \"{title}\".")
    } else {
        "Facciamo esercizi.".to_string()
    };
    if note.is_empty() {
        head
    } else {
        format!("{head} {note}")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

    const TITLE: &str = "Il titolo pianificato della lezione due";
    const DAG: &str = "graph TD\n  N1[\"Nodo 1: Prima cosa\"] --> N2[\"Nodo 2: Seconda cosa\"]\n  N2 --> N3[\"Nodo 3: Terza cosa\"]";
    const DAY: &str = "2026-10-27T09:00:00.000Z";

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("proxi-context-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn write_lesson(data_root: &Path, course: &str, filename: &str, doc: &Value) {
        let dir = data_root.join("courses").join(course).join("lessons");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(filename),
            format!(r#"<script type="application/json" id="lesson-data">{doc}</script>"#),
        )
        .unwrap();
    }

    fn make_repo(tag: &str, bare: bool) -> (TempDir, PathBuf, String) {
        let dir = temp_dir(tag);
        let root = dir.0.clone();
        let real = crate::data_root::repo_root();
        let docs = root.join("docs").join("agents");
        fs::create_dir_all(&docs).unwrap();
        for name in ["pedagogy.md", "anki-cards.md"] {
            fs::copy(real.join("docs").join("agents").join(name), docs.join(name)).unwrap();
        }
        let course = root.join("courses").join("demo");
        fs::create_dir_all(course.join("learning-records")).unwrap();
        fs::create_dir_all(course.join("lessons")).unwrap();
        fs::write(
            root.join("curriculum.json"),
            json!({
                "chapters": [{
                    "title": "Demo",
                    "slug": "demo",
                    "lessons": [
                        { "title": "Lezione uno", "slug": "lezione-uno", "plannedNodes": 3 },
                        { "title": TITLE, "slug": "il-titolo-pianificato-della-lezione-due", "plannedNodes": 4 },
                    ],
                }],
            })
            .to_string(),
        )
        .unwrap();
        if !bare {
            fs::write(course.join("MISSION.md"), "# Mission\nMISSION-MARKER").unwrap();
            fs::write(course.join("NOTES.md"), "# Notes\nNOTES-MARKER").unwrap();
            for n in 1..=4 {
                fs::write(
                    course
                        .join("learning-records")
                        .join(format!("000{n}-x{n}.md")),
                    format!("# rec {n}\nREC-{n}"),
                )
                .unwrap();
            }
        }
        let doc = json!({
            "metadata": {
                "lessonNumber": 1,
                "topic": "Demo topic",
                "title": "Lezione uno",
                "slug": "lezione-uno",
                "dateCreated": DAY,
                "dateModified": DAY,
                "initialDag": DAG,
                "targetGoal": "GOAL-MARKER",
                "version": "1.0.0",
            },
            "nodes": [{
                "nodeId": "prima-cosa",
                "title": "Prima cosa",
                "tier": "unconditional-truth",
                "content": "<p>x</p>",
            }],
        });
        write_lesson(&root, "demo", "0001-lezione-uno.html", &doc);
        (
            dir,
            root,
            "courses/demo/lessons/0001-lezione-uno.html".to_string(),
        )
    }

    fn config(value: Value) -> Map<String, Value> {
        value.as_object().expect("config object").clone()
    }

    fn teach_planned(anki: bool) -> Map<String, Value> {
        config(json!({
            "kind": "teach",
            "course": "demo",
            "model": "sonnet",
            "target": { "mode": "planned", "title": TITLE },
            "ankiOptIn": anki,
        }))
    }

    fn teach_standalone() -> Map<String, Value> {
        config(json!({
            "kind": "teach",
            "course": "lezioni-singole",
            "model": "sonnet",
            "target": { "mode": "free" },
            "ankiOptIn": false,
        }))
    }

    fn plan_demo() -> Map<String, Value> {
        config(json!({
            "kind": "plan",
            "course": "demo",
            "model": "sonnet",
            "chapterTitle": "Demo",
        }))
    }

    fn practice_cfg(extra: Value) -> Map<String, Value> {
        let mut cfg = config(json!({
            "kind": "practice",
            "course": "demo",
            "model": "sonnet",
            "practiceNodes": [],
        }));
        if let Some(extra) = extra.as_object() {
            for (key, value) in extra {
                cfg.insert(key.clone(), value.clone());
            }
        }
        cfg
    }

    fn build(root: &Path, config: &Map<String, Value>, lesson_path: Option<&str>) -> String {
        let code_root = crate::data_root::repo_root();
        build_system_prompt(&PromptInput {
            config,
            code_root: &code_root,
            repo_root: root,
            data_root: None,
            lesson_path,
            today: "2026-10-27",
        })
    }

    /// `a` then at least one JS whitespace char, then `b`.
    fn has_ws_gap(text: &str, a: &str, b: &str) -> bool {
        let mut from = 0;
        while let Some(pos) = text[from..].find(a) {
            let start = from + pos + a.len();
            let mut idx = start;
            let mut saw_ws = false;
            while let Some(c) = text[idx..].chars().next() {
                if js::is_js_ws(c) {
                    saw_ws = true;
                    idx += c.len_utf8();
                } else {
                    break;
                }
            }
            if saw_ws && text[idx..].starts_with(b) {
                return true;
            }
            from = start;
        }
        false
    }

    /// `tra le` + JS whitespace + `premesse`.
    fn tra_le_premesse(text: &str) -> bool {
        let mut from = 0;
        while let Some(pos) = text[from..].find("tra le") {
            let start = from + pos + "tra le".len();
            let mut idx = start;
            let mut saw_ws = false;
            while let Some(c) = text[idx..].chars().next() {
                if js::is_js_ws(c) {
                    saw_ws = true;
                    idx += c.len_utf8();
                } else {
                    break;
                }
            }
            if saw_ws && text[idx..].starts_with("premesse") {
                return true;
            }
            from = start;
        }
        false
    }

    /// The body under a markdown heading marker, up to the next `##`/`###` heading.
    fn section_after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
        let start = text.find(marker)?;
        let rest = &text[start + 1..];
        let end = [rest.find("\n## "), rest.find("\n### ")]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(rest.len());
        Some(&rest[..end])
    }

    fn real_file(rel: &str) -> String {
        let root = crate::data_root::repo_root();
        fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
    }

    fn graph_line(i: usize, chat_id: &str) -> String {
        json!({
            "id": format!("n_g{i}"),
            "name": format!("Concetto {i}"),
            "statement": format!("La proposizione numero {i} del grafo."),
            "aliases": [],
            "createdAt": "2026-01-01T00:00:00.000Z",
            "provenance": [{ "chatId": chat_id, "messageId": "prima-cosa" }],
        })
        .to_string()
    }

    fn write_progress(root: &Path, records: &[Value]) {
        let dir = root.join("courses").join("demo");
        fs::create_dir_all(&dir).unwrap();
        let text = records
            .iter()
            .map(|record| record.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(dir.join("progress.jsonl"), text).unwrap();
    }

    fn anki_rec(node_id: &str, at: &str, ease: f64) -> Value {
        json!({
            "at": at,
            "sessionId": "anki",
            "course": "demo",
            "lessonSlug": "lezione-uno",
            "nodeRefs": [format!("demo/lezione-uno#{node_id}")],
            "kind": "review",
            "source": "anki",
            "reviewId": 1,
            "cardId": 1,
            "ease": ease,
            "lapses": 0,
            "interval": 0,
        })
    }

    #[test]
    fn teach_planned_has_sections_title_anki_line_course_files_and_last_records() {
        let (_dir, root, _rel) = make_repo("teach-planned", false);
        let p = build(&root, &teach_planned(true), None);
        assert!(p.contains("# Who you are"), "{p}");
        assert!(p.contains("# This is a teaching session"));
        assert!(p.contains("# Course context"));
        assert!(p.contains("# This session"));
        assert!(p.contains(&format!("\"{TITLE}\"")));
        assert!(p.contains("Anki opt-in: Sì"));
        assert!(p.contains("Today: 2026-10-27"));
        assert!(p.contains("<file path=\"courses/demo/MISSION.md\">"));
        assert!(p.contains("NOTES-MARKER"));
        assert!(
            !p.contains("REC-1") && p.contains("REC-2") && p.contains("REC-4"),
            "{p}"
        );
        assert!(
            has_ws_gap(&p, "[ ] ----", "Il titolo pianificato"),
            "missing planned lesson line"
        );
        assert!(p.contains("anki-cards.md"));
    }

    #[test]
    fn anki_no_omits_the_card_reference() {
        let (_dir, root, _rel) = make_repo("anki-no", false);
        let p = build(&root, &teach_planned(false), None);
        assert!(p.contains("Anki opt-in: No"));
        assert!(!p.contains("path=\"docs/agents/anki-cards.md\""));
    }

    #[test]
    fn existing_lesson_lists_captured_nodes_and_the_first_missing_plan_node() {
        let (_dir, root, rel) = make_repo("existing", false);
        let cfg = config(json!({
            "kind": "teach",
            "course": "demo",
            "model": "sonnet",
            "target": { "mode": "existing", "lessonPath": rel },
            "ankiOptIn": false,
        }));
        let p = build(&root, &cfg, Some(&rel));
        assert!(p.contains("GOAL-MARKER"));
        assert!(p.contains("N1: nodeId \"prima-cosa\""), "{p}");
        assert!(p.contains("Captured nodes (1 of 3 planned)"));
        assert!(p.contains("First missing plan node: N2"));
        assert!(p.contains("Nodo 3: Terza cosa"));
    }

    #[test]
    fn missing_files_are_tolerated() {
        let (_dir, root, _rel) = make_repo("bare", true);
        fs::remove_dir_all(root.join("docs")).unwrap();
        let cfg = config(json!({
            "kind": "teach",
            "course": "demo",
            "model": "haiku",
            "target": { "mode": "free" },
            "ankiOptIn": false,
        }));
        let p = build(&root, &cfg, None);
        assert!(p.contains("Target: free"));
        assert!(!p.contains("MISSION-MARKER"));
    }

    #[test]
    fn plan_session_on_a_new_course_and_practice_session() {
        let (_dir, root, _rel) = make_repo("plan-practice", false);
        let plan = build(
            &root,
            &config(json!({
                "kind": "plan",
                "course": "nuovo",
                "model": "sonnet",
                "chapterTitle": "Nuovo corso",
            })),
            None,
        );
        assert!(plan.contains("# This is a planning session"));
        assert!(plan.contains("is new: provide a full mission"));
        let practice = build(&root, &practice_cfg(json!({})), None);
        assert!(practice.contains("# This is an exercise session (Esercizi)"));
        assert!(practice.contains("Course nodes (pass the key as problem.nodeKeys):"));
        assert!(practice.contains("- key \"demo/lezione-uno#prima-cosa\" | Prima cosa | status"));
        assert!(!practice.contains("Reference documents"));
    }

    #[test]
    fn opening_message_matches_the_target() {
        assert_eq!(
            opening_message(&teach_planned(true), None),
            format!("Iniziamo la lezione \"{TITLE}\".")
        );
        let plan = config(json!({
            "kind": "plan",
            "course": "c",
            "model": "sonnet",
            "chapterTitle": "X",
            "note": "ciao",
        }));
        assert_eq!(
            opening_message(&plan, None),
            "Pianifichiamo il corso \"X\". ciao"
        );
        let practice = config(json!({ "kind": "practice", "course": "c", "model": "sonnet" }));
        assert!(opening_message(&practice, None).starts_with("Facciamo esercizi."));
    }

    #[test]
    fn reference_preface_states_that_app_tools_override_the_docs() {
        let (_dir, root, _rel) = make_repo("preface", false);
        let p = build(&root, &teach_planned(false), None);
        assert!(p.contains("the app tools and the `# This session` block override them"));
        assert!(p.contains("mirror quiz in a node artifact is not"));
    }

    #[test]
    fn every_kind_carries_the_course_index() {
        let (_dir, root, _rel) = make_repo("course-index", false);
        let configs = [
            teach_planned(false),
            teach_standalone(),
            plan_demo(),
            practice_cfg(json!({})),
        ];
        for cfg in &configs {
            let p = build(&root, cfg, None);
            assert!(
                p.contains("# Course index"),
                "no course index for {:?}",
                cfg.get("kind")
            );
            let demo = p
                .split('\n')
                .find(|line| line.starts_with("- Demo (courses/demo) | lessons "))
                .unwrap_or_else(|| panic!("no Demo row for {:?}:\n{p}", cfg.get("kind")));
            assert!(
                demo.contains(" | solido ")
                    && demo.contains(", fragile ")
                    && demo.contains(", insegnato "),
                "{demo}"
            );
            assert!(
                p.split('\n').any(|line| line
                    .starts_with("- Lezioni singole (courses/lezioni-singole) | lessons ")),
                "no Lezioni singole line for {:?}",
                cfg.get("kind")
            );
        }

        // the practice paths that skip the opening question still get the index
        for extra in [
            json!({ "practicePropositions": ["n_x"] }),
            json!({ "practiceNodes": ["missing#x"] }),
        ] {
            let p = build(&root, &practice_cfg(extra), None);
            assert!(p.contains("# Course index"));
        }
    }

    #[test]
    fn prompt_length_does_not_follow_the_graph_size() {
        let (_dir, root, _rel) = make_repo("graph-size", false);
        fs::create_dir_all(root.join("graph")).unwrap();
        let chat_id = "demo/0001-lezione-uno";
        let configs = [teach_planned(true), plan_demo(), practice_cfg(json!({}))];
        let build_all = || {
            configs
                .iter()
                .map(|cfg| build(&root, cfg, None))
                .collect::<Vec<_>>()
        };
        let write = |n: usize| {
            let lines: Vec<String> = (0..n).map(|i| graph_line(i, chat_id)).collect();
            fs::write(
                root.join("graph").join("nodes.jsonl"),
                lines.join("\n") + "\n",
            )
            .unwrap();
        };

        write(42);
        let small = build_all();
        write(2000);
        let big = build_all();
        for i in 0..configs.len() {
            assert_eq!(
                big[i].len(),
                small[i].len(),
                "{:?} prompt grew with the graph",
                configs[i].get("kind")
            );
        }
    }

    #[test]
    fn no_removed_concept_list_reaches_the_prompt() {
        let (_dir, root, _rel) = make_repo("removed", false);
        write_progress(
            &root,
            &[anki_rec("prima-cosa", "2026-01-01T00:00:00.000Z", 1.0)],
        );
        let configs = [
            teach_planned(true),
            teach_standalone(),
            plan_demo(),
            practice_cfg(json!({})),
        ];
        let mut prompts = Vec::new();
        for cfg in &configs {
            prompts.push(build(&root, cfg, None));
        }

        let removed = [
            "Cards already in this course",
            "Concepts to reinforce",
            "Weak in his Anki reviews",
            "No nodes selected: choose from the course map",
            "Anki cards of this course",
        ];
        for p in &prompts {
            for text in removed {
                assert!(!p.contains(text), "\"{text}\" still in the prompt");
            }
        }

        let practice = &prompts[3];
        assert!(practice.contains("Opening: nothing picked on the map."));
        assert!(practice.contains("call mcp__app__browse_concepts on this course"));
        assert!(practice.contains("Course nodes (pass the key as problem.nodeKeys):"));
        assert!(
            practice
                .split('\n')
                .any(|line| line.starts_with("- key \"demo/"))
        );
    }

    #[test]
    fn practice_picked_map_items_skip_the_opening_question() {
        let (_dir, root, _rel) = make_repo("practice-picked", false);
        let mut p = build(
            &root,
            &practice_cfg(json!({ "practicePropositions": ["n_x"] })),
            None,
        );
        assert!(p.contains("Opening: he picked these on the map."));
        assert!(!p.contains("Opening: nothing picked on the map."));

        p = build(
            &root,
            &practice_cfg(json!({ "practiceNodes": ["demo/lezione-uno#prima-cosa"] })),
            None,
        );
        assert!(p.contains("Opening: he picked these on the map."));
        assert!(p.contains("key \"demo/lezione-uno#prima-cosa\""));
        assert!(!p.contains("Opening: nothing picked on the map."));

        // picked keys the map no longer has: still his picks, so still no opening question
        p = build(
            &root,
            &practice_cfg(json!({ "practiceNodes": ["demo/lezione-uno#sparito"] })),
            None,
        );
        assert!(p.contains("not found in the map"));
        assert!(p.contains("Opening: he picked these on the map."));
        assert!(!p.contains("Opening: nothing picked on the map."));
    }

    #[test]
    fn phase_1a_rule_and_card_rule_in_the_teach_prompt_concept_tools_in_every_prompt() {
        let teach = real_file("app/prompts/teach.md");
        let practice = real_file("app/prompts/practice.md");
        let plan = real_file("app/prompts/plan.md");
        assert!(teach.contains("Before the Phase 1a probe, consult his knowledge"));
        assert!(teach.contains("Before authoring cards, call `mcp__app__search_concepts`"));
        assert!(practice.contains("Before your opening question, consult the concept tools"));
        assert!(plan.contains("Before probing, read the **Course index**"));
        for text in [&teach, &practice, &plan] {
            for tool in [
                "mcp__app__search_concepts",
                "mcp__app__open_concept",
                "mcp__app__browse_concepts",
            ] {
                assert!(text.contains(tool), "missing {tool}");
            }
        }
    }

    #[test]
    fn write_node_payload_documents_anchors_to_known_concepts() {
        let teach = real_file("app/prompts/teach.md");
        let section =
            section_after(&teach, "### `write_node` payload").expect("write_node payload section");
        assert!(section.contains("`anchors`"));
        assert!(section.contains("n_…"));
        assert!(section.contains("search_concepts"));
        assert!(section.contains("Every node gets at least one anchor"));
        assert!(section.contains("names each anchored concept with its stored name verbatim"));
        assert!(section.contains("resend its `anchors`"));
    }

    #[test]
    fn the_teach_cadence_anchors_every_node_to_the_concept_kept_in_phase_1a() {
        let teach = real_file("app/prompts/teach.md");
        assert!(teach.contains("keep its `n_…` id for the node's `anchors`"));
        assert!(teach.contains("Every node carries `anchors`"));
    }

    #[test]
    fn the_concept_search_guidance_asks_for_bare_name_one_concept_at_a_time_queries() {
        let teach = real_file("app/prompts/teach.md");
        let probe = teach.find("Before the Phase 1a probe").expect("probe rule");
        let probe_end = teach[probe..]
            .find("**1b.")
            .map(|end| probe + end)
            .expect("1b heading");
        let section = &teach[probe..probe_end];
        assert!(section.contains("one concept at a time by its bare name"));
        assert!(section.contains(
            "whole-phrase query hides the partial matches behind any record that holds every word"
        ));

        let payload =
            section_after(&teach, "### `write_node` payload").expect("write_node payload section");
        assert!(payload.contains("only if those searches return nothing relevant, omit `anchors`"));
    }

    #[test]
    fn map_prompt_tells_the_extractor_to_use_the_tutors_declared_anchors() {
        let map = real_file("app/prompts/subagents/map.md");
        assert!(map.contains("**ancore dichiarate dal tutor**"));
        assert!(tra_le_premesse(&map), "missing \"tra le premesse\"");
        assert!(map.contains("aggiunta in automatico"));
    }
}
