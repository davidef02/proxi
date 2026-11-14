//! The course index block appended to the system prompt (port of `app/server/courseindex.ts`).
//!
//! One line per course of what he already knows; counts come from lesson nodes, never from graph
//! propositions, so the prompt stays the same length however big the graph grows.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use serde_json::Value;

use crate::course_data::{STANDALONE_COURSE_SLUG, STANDALONE_COURSE_TITLE};
use crate::courses::{self, Chapter, Match};
use crate::graph::store::GraphStore;
use crate::js;
use crate::knowledge::{self, MapNode, Status};
use crate::lesson::{self, Lesson};

struct Counts {
    solid: usize,
    shaky: usize,
    taught: usize,
    missed: usize,
}

fn counts_of(nodes: &[MapNode], slug: &str) -> Counts {
    let mut counts = Counts {
        solid: 0,
        shaky: 0,
        taught: 0,
        missed: 0,
    };
    for node in nodes {
        if node.json.get("course").and_then(Value::as_str) != Some(slug) {
            continue;
        }
        match node.status {
            Status::Solid => counts.solid += 1,
            Status::Shaky => counts.shaky += 1,
            Status::Taught => counts.taught += 1,
            Status::Missed => counts.missed += 1,
        }
    }
    counts
}

/// `(dateModified || dateCreated || "").slice(0, 10)`.
fn lesson_date(lesson: &Lesson) -> String {
    let metadata = lesson.doc.get("metadata").unwrap_or(&Value::Null);
    for key in ["dateModified", "dateCreated"] {
        if let Some(value) = metadata.get(key) {
            if js::truthy(Some(value)) {
                return js::display(Some(value)).chars().take(10).collect();
            }
        }
    }
    String::new()
}

fn last_lesson(lessons: &[Lesson], slug: &str) -> String {
    let mut last = String::new();
    for lesson in lessons {
        if lesson.summary.course != slug {
            continue;
        }
        let date = lesson_date(lesson);
        if date > last {
            last = date;
        }
    }
    if last.is_empty() {
        "-".to_string()
    } else {
        last
    }
}

fn row(
    title: &str,
    slug: &str,
    lessons_text: &str,
    nodes: &[MapNode],
    lessons: &[Lesson],
) -> String {
    let counts = counts_of(nodes, slug);
    format!(
        "- {title} (courses/{slug}) | lessons {lessons_text} | solido {}, fragile {}, insegnato {}, mancato {} | last lesson {}",
        counts.solid,
        counts.shaky,
        counts.taught,
        counts.missed,
        last_lesson(lessons, slug)
    )
}

pub fn course_index_block(data_root: &Path) -> String {
    // Every read is guarded: a broken file must not stop a session from starting.
    let lessons = lesson::list_lessons(data_root);
    let chapters: Vec<Chapter> = courses::read_curriculum(data_root).unwrap_or_default();
    let nodes: Vec<MapNode> = match knowledge::build_knowledge_map(data_root) {
        Ok(map) => map.nodes,
        Err(_) => Vec::new(),
    };
    let graph_empty = GraphStore::new(data_root.to_path_buf())
        .read_node_lines()
        .is_empty();
    let matched: Match = courses::match_curriculum(&chapters, &lessons);

    let mut rows: Vec<String> = Vec::new();
    let mut planned: HashSet<String> = HashSet::new();
    for (chapter_idx, entries) in &matched.chapters {
        let chapter = &chapters[*chapter_idx];
        let slug = js::js_string(chapter.slug.as_ref());
        planned.insert(slug.clone());
        if slug == STANDALONE_COURSE_SLUG {
            continue;
        }
        let taught = entries
            .iter()
            .filter(|(_, lesson_idx)| lesson_idx.is_some())
            .count();
        let orphans = matched
            .orphans
            .iter()
            .filter(|&&idx| lessons[idx].summary.course == slug)
            .count();
        rows.push(row(
            &js::display(chapter.title.as_ref()),
            &slug,
            &format!("{}/{}", taught + orphans, entries.len()),
            &nodes,
            &lessons,
        ));
    }

    let mut extras: BTreeSet<String> = BTreeSet::new();
    for &idx in &matched.orphans {
        let slug = &lessons[idx].summary.course;
        if slug != STANDALONE_COURSE_SLUG && !planned.contains(slug) {
            extras.insert(slug.clone());
        }
    }
    for slug in extras {
        let done = matched
            .orphans
            .iter()
            .filter(|&&idx| lessons[idx].summary.course == slug)
            .count();
        rows.push(row(
            &slug,
            &slug,
            &format!("{done} (no plan)"),
            &nodes,
            &lessons,
        ));
    }

    let standalone = lessons
        .iter()
        .filter(|lesson| lesson.summary.course == STANDALONE_COURSE_SLUG)
        .count();
    rows.push(row(
        STANDALONE_COURSE_TITLE,
        STANDALONE_COURSE_SLUG,
        &format!("{standalone} (no plan)"),
        &nodes,
        &lessons,
    ));

    let mut lines: Vec<String> = vec![
        "# Course index".to_string(),
        String::new(),
        "What he already knows, one line per course (concept counts by status). Look concepts up with `mcp__app__search_concepts`, `mcp__app__open_concept` and `mcp__app__browse_concepts`; their details are not in this prompt.".to_string(),
        String::new(),
    ];
    lines.extend(rows);
    if graph_empty {
        lines.push(String::new());
        lines.push(
            "The knowledge graph is empty: the concept tools return nothing yet.".to_string(),
        );
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

    const STANDALONE_LINE: &str = "- Lezioni singole (courses/lezioni-singole) | lessons 0 (no plan) | solido 0, fragile 0, insegnato 0, mancato 0 | last lesson -";

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "proxi-courseindex-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn lesson_doc(number: i64, title: &str, slug: &str, node_ids: &[&str]) -> Value {
        json!({
            "metadata": {
                "lessonNumber": number,
                "topic": "Demo",
                "title": title,
                "slug": slug,
                "dateCreated": "2026-01-01T00:00:00.000Z",
                "dateModified": "2026-01-01T00:00:00.000Z",
                "initialDag": "",
                "version": "1.0.0",
            },
            "nodes": node_ids.iter().map(|node_id| json!({
                "nodeId": node_id,
                "title": node_id,
                "tier": "unconditional-truth",
                "content": "<p>x</p>",
            })).collect::<Vec<_>>(),
        })
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

    fn append_progress(data_root: &Path, record: Value) {
        let dir = data_root.join("courses").join("demo");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("progress.jsonl");
        let mut line = record.to_string();
        line.push('\n');
        let mut open = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
            .unwrap();
        open.write_all(line.as_bytes()).unwrap();
    }

    /// Demo has two planned lessons (one taught) and is solid on its only node; extra has one
    /// orphan lesson.
    fn make_repo(tag: &str) -> (TempDir, PathBuf, PathBuf) {
        let dir = temp_dir(tag);
        let root = dir.0.clone();
        fs::write(
            root.join("curriculum.json"),
            json!({
                "chapters": [{
                    "title": "Demo",
                    "slug": "demo",
                    "lessons": [
                        { "title": "Lezione uno", "slug": "lezione-uno", "plannedNodes": 1 },
                        { "title": "Lezione due", "slug": "lezione-due", "plannedNodes": 1 },
                    ],
                }],
            })
            .to_string(),
        )
        .unwrap();

        let demo = lesson_doc(1, "Lezione uno", "lezione-uno", &["prima-cosa"]);
        write_lesson(&root, "demo", "0001-lezione-uno.html", &demo);
        let extra = lesson_doc(1, "Lezione extra", "lezione-extra", &["extra-cosa"]);
        write_lesson(&root, "extra", "0001-lezione-extra.html", &extra);

        append_progress(
            &root,
            json!({
                "at": "2026-01-01T00:00:00.000Z",
                "sessionId": "s",
                "course": "demo",
                "lessonSlug": "lezione-uno",
                "nodeRefs": ["demo/lezione-uno#prima-cosa"],
                "kind": "check",
                "correct": true,
                "confidence": 90,
            }),
        );

        let demo_path = root
            .join("courses")
            .join("demo")
            .join("lessons")
            .join("0001-lezione-uno.html");
        (dir, root, demo_path)
    }

    #[test]
    fn one_line_per_course_planned_orphan_and_lezioni_singole() {
        let (_dir, root, _demo) = make_repo("one-line");
        let block = course_index_block(&root);

        assert!(block.starts_with("# Course index\n"), "{block}");
        let lines: Vec<&str> = block.split('\n').collect();
        assert!(
            lines.contains(&"- Demo (courses/demo) | lessons 1/2 | solido 1, fragile 0, insegnato 0, mancato 0 | last lesson 2026-01-01"),
            "{block}"
        );
        assert!(
            lines.contains(&"- extra (courses/extra) | lessons 1 (no plan) | solido 0, fragile 0, insegnato 1, mancato 0 | last lesson 2026-01-01"),
            "{block}"
        );
        assert!(lines.contains(&STANDALONE_LINE), "{block}");

        let at = |needle: &str| {
            block
                .split('\n')
                .position(|line| line.starts_with(needle))
                .unwrap_or_else(|| panic!("missing {needle}:\n{block}"))
        };
        assert!(at("- Demo (") < at("- extra ("));
        assert!(at("- extra (") < at("- Lezioni singole ("));

        // no writes on read: the standalone course is never created
        assert!(!root.join("courses").join("lezioni-singole").exists());
    }

    #[test]
    fn empty_graph_note_is_gone_once_a_proposition_exists() {
        let (_dir, root, _demo) = make_repo("graph-note");
        let block = course_index_block(&root);
        assert!(
            block.contains("The knowledge graph is empty: the concept tools return nothing yet.")
        );

        let graph = root.join("graph");
        fs::create_dir_all(&graph).unwrap();
        fs::write(
            graph.join("nodes.jsonl"),
            json!({
                "id": "n_fx",
                "name": "Prima cosa",
                "statement": "La prima cosa è così.",
                "aliases": [],
                "createdAt": "2026-01-01T00:00:00.000Z",
                "provenance": [{ "chatId": "demo/0001-lezione-uno", "messageId": "prima-cosa" }],
            })
            .to_string()
                + "\n",
        )
        .unwrap();
        let block = course_index_block(&root);
        assert!(!block.contains("The knowledge graph is empty"), "{block}");
    }

    #[test]
    fn a_malformed_curriculum_json_does_not_break_the_block() {
        let (_dir, root, _demo) = make_repo("malformed");
        fs::write(root.join("curriculum.json"), "{\"chapters\": 5}").unwrap();

        let block = course_index_block(&root);
        assert!(block.starts_with("# Course index\n"));
        assert!(block.split('\n').any(|line| line == STANDALONE_LINE));
        // both courses fall back to orphan rows
        assert!(
            block
                .split('\n')
                .any(|line| line.starts_with("- demo (courses/demo) | lessons 1 (no plan) |")),
            "{block}"
        );
    }

    #[test]
    fn counts_every_status_separately() {
        let (_dir, root, demo) = make_repo("statuses");
        let doc = lesson_doc(
            1,
            "Lezione uno",
            "lezione-uno",
            &["prima-cosa", "seconda", "terza", "quarta"],
        );
        write_lesson(&root, "demo", "0001-lezione-uno.html", &doc);
        assert!(demo.exists());

        let base = json!({
            "sessionId": "s",
            "course": "demo",
            "lessonSlug": "lezione-uno",
            "kind": "check",
        });
        let mut second = base.clone();
        second["nodeRefs"] = json!(["demo/lezione-uno#seconda"]);
        second["correct"] = json!(true);
        second["confidence"] = json!(20);
        second["at"] = json!("2026-01-02T00:00:00.000Z");
        append_progress(&root, second);
        let mut third = base.clone();
        third["nodeRefs"] = json!(["demo/lezione-uno#terza"]);
        third["correct"] = json!(false);
        third["at"] = json!("2026-01-03T00:00:00.000Z");
        append_progress(&root, third);

        let block = course_index_block(&root);
        assert!(
            block
                .split('\n')
                .any(|line| line == "- Demo (courses/demo) | lessons 1/2 | solido 1, fragile 1, insegnato 1, mancato 1 | last lesson 2026-01-01"),
            "{block}"
        );
    }

    #[test]
    fn a_lezioni_singole_chapter_in_the_plan_is_not_duplicated() {
        let (_dir, root, _demo) = make_repo("standalone-chapter");
        fs::write(
            root.join("curriculum.json"),
            json!({
                "chapters": [
                    {
                        "title": "Demo",
                        "slug": "demo",
                        "lessons": [
                            { "title": "Lezione uno", "slug": "lezione-uno", "plannedNodes": 1 },
                            { "title": "Lezione due", "slug": "lezione-due", "plannedNodes": 1 },
                        ],
                    },
                    {
                        "title": "Lezioni singole",
                        "slug": "lezioni-singole",
                        "lessons": [{ "title": "Sola", "slug": "sola", "plannedNodes": 1 }],
                    },
                ],
            })
            .to_string(),
        )
        .unwrap();
        let sola = lesson_doc(1, "Sola", "sola", &["sola-cosa"]);
        write_lesson(&root, "lezioni-singole", "0001-sola.html", &sola);

        let block = course_index_block(&root);
        let standalone: Vec<&str> = block
            .split('\n')
            .filter(|line| line.starts_with("- Lezioni singole (courses/lezioni-singole)"))
            .collect();
        assert_eq!(standalone.len(), 1, "{block}");
        assert!(
            standalone[0].contains(" | lessons 1 (no plan) | "),
            "{block}"
        );
    }
}
