//! The plan: `curriculum.json` and its join onto what has been taught; mirrors `extensions/chapter.ts`.
//!
//! A chapter is a course: `courses/<slug>/`. The chapter is the only place a lesson that does
//! not exist yet can be named, so this module owns `curriculum.json` end to end: writing it,
//! reading it, and matching its entries against the lessons actually on disk.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::data_root;
use crate::slug::{js_trim, lesson_slug, slugify};

/// A lesson named by the plan. It has no file until it has been taught.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedLesson {
    pub title: String,
    pub slug: String,
    pub planned_nodes: Option<f64>,
}

/// A lesson that exists on disk, summarized from its data island. Lives here so `chapter.rs`
/// does not depend on `homepage.rs`.
#[derive(Debug, Clone, PartialEq)]
pub struct LessonSummary {
    pub course: String,
    pub lesson_number: i64,
    pub slug: String,
    pub title: String,
    pub filename: String,
    pub node_count: usize,
    pub tiers: Vec<String>,
    pub has_reference: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Curriculum {
    pub chapters: Vec<Map<String, Value>>,
}

/// A plan entry, with the taught lesson claiming it if there is one.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchedLesson {
    pub planned: PlannedLesson,
    pub lesson: Option<LessonSummary>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchedChapter {
    pub title: String,
    pub slug: String,
    pub lessons: Vec<MatchedLesson>,
    pub taught: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CurriculumMatch {
    pub chapters: Vec<MatchedChapter>,
    /// Every taught lesson, matched or not.
    pub taught: Vec<LessonSummary>,
    /// Taught lessons no plan entry claims. Computed once, here.
    pub orphans: Vec<LessonSummary>,
}

pub fn curriculum_file(base_dir: &Path, curriculum_path: Option<&str>) -> PathBuf {
    match curriculum_path {
        Some(path) if !path.is_empty() => data_root::resolve_path_from(base_dir, Path::new(path)),
        _ => base_dir.join("curriculum.json"),
    }
}

pub fn read_curriculum(file: &Path) -> Result<Curriculum, String> {
    if !file.exists() {
        return Ok(Curriculum::default());
    }
    let raw = fs::read_to_string(file).map_err(|e| e.to_string())?;
    let parsed: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let Some(object) = parsed.as_object() else {
        return Ok(Curriculum::default());
    };
    let Some(chapters) = object.get("chapters") else {
        return Ok(Curriculum::default());
    };
    let Some(chapters) = chapters.as_array() else {
        // Refuse a wrong shape instead of degrading it to []: plan_chapter writes the result
        // back, so silently reading a malformed file as "no chapters" would erase the plan.
        return Err(format!(
            "Malformed {}: \"chapters\" must be an array",
            file.display()
        ));
    };
    let chapters = chapters
        .iter()
        .map(|c| {
            if let Some(chapter) = c.as_object() {
                let mut chapter = chapter.clone();
                let lessons = chapter
                    .get("lessons")
                    .filter(|value| value.is_array())
                    .cloned()
                    .unwrap_or(Value::Array(Vec::new()));
                // insert keeps an existing key's position and appends when there was none.
                chapter.insert("lessons".to_string(), lessons);
                chapter
            } else {
                let mut chapter = Map::new();
                chapter.insert("lessons".to_string(), Value::Array(Vec::new()));
                chapter
            }
        })
        .collect();
    Ok(Curriculum { chapters })
}

pub fn chapter_title(chapter: &Map<String, Value>) -> String {
    field_string(chapter, "title")
}

pub fn chapter_slug(chapter: &Map<String, Value>) -> String {
    field_string(chapter, "slug")
}

pub fn planned_lessons(chapter: &Map<String, Value>) -> Vec<PlannedLesson> {
    let Some(lessons) = chapter.get("lessons").and_then(Value::as_array) else {
        return Vec::new();
    };
    lessons
        .iter()
        .map(|lesson| {
            let object = lesson.as_object();
            PlannedLesson {
                title: object
                    .and_then(|o| o.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                slug: object
                    .and_then(|o| o.get("slug"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                // only a JSON number counts; booleans and strings are not planned node counts
                planned_nodes: object
                    .and_then(|o| o.get("plannedNodes"))
                    .and_then(Value::as_f64),
            }
        })
        .collect()
}

/// JS `String(number)`, for the plan's node counts.
pub fn js_number(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e21 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// Upserts one chapter's lesson list. Re-planning a chapter replaces its lessons wholesale:
/// the plan is mutable by design, and git keeps the history.
pub fn plan_chapter(
    file: &Path,
    chapter: &str,
    slug: Option<&str>,
    lessons: &[Value],
) -> Result<Curriculum, String> {
    let title = js_trim(chapter);
    if title.is_empty() {
        return Err("A chapter title is required".to_string());
    }
    let mut curriculum = read_curriculum(file)?;
    let course_slug = match slug {
        Some(explicit) if !explicit.is_empty() => slugify(explicit),
        _ => slugify(chapter),
    };

    let mut planned = Vec::with_capacity(lessons.len());
    for lesson in lessons {
        let object = lesson.as_object();
        let lesson_title = object
            .and_then(|o| o.get("title"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let trimmed = js_trim(lesson_title);
        if trimmed.is_empty() {
            return Err("Every planned lesson needs a title".to_string());
        }
        let explicit_slug = object
            .and_then(|o| o.get("slug"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        // lesson_slug uses the title as given; slugify trims anyway.
        let slug = if explicit_slug.is_empty() {
            lesson_slug(lesson_title)
        } else {
            slugify(explicit_slug)
        };

        let mut planned_lesson = Map::new();
        planned_lesson.insert("title".to_string(), Value::String(trimmed.to_string()));
        planned_lesson.insert("slug".to_string(), Value::String(slug));
        if let Some(Value::Number(number)) = object.and_then(|o| o.get("plannedNodes")) {
            planned_lesson.insert("plannedNodes".to_string(), Value::Number(number.clone()));
        }
        planned.push(Value::Object(planned_lesson));
    }

    // Two entries with the same slug would both match the one artifact on disk, and the chapter
    // would read as complete with a lesson never taught. Titles can collide through slugify.
    let mut seen: HashMap<String, String> = HashMap::new();
    for lesson in &planned {
        let slug = lesson
            .get("slug")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let title = lesson
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some(first) = seen.get(&slug) {
            return Err(format!(
                "Two planned lessons share the slug \"{slug}\": \"{first}\" and \"{title}\". Give them distinct titles, or pass an explicit slug."
            ));
        }
        seen.insert(slug, title);
    }

    let mut new_chapter = Map::new();
    new_chapter.insert("title".to_string(), Value::String(title.to_string()));
    new_chapter.insert("slug".to_string(), Value::String(course_slug.clone()));
    new_chapter.insert("lessons".to_string(), Value::Array(planned));
    match curriculum
        .chapters
        .iter()
        .position(|c| chapter_slug(c) == course_slug)
    {
        Some(index) => curriculum.chapters[index] = new_chapter,
        None => curriculum.chapters.push(new_chapter),
    }

    if let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let document = serde_json::json!({ "chapters": &curriculum.chapters });
    let mut text = serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?;
    text.push('\n');
    fs::write(file, text).map_err(|e| e.to_string())?;
    Ok(curriculum)
}

/// Joins the plan onto the lessons on disk. A plan entry is claimed only by a lesson in that
/// chapter's own course directory, so two courses may hold same-named lessons without stealing
/// each other's entries.
pub fn match_curriculum(curriculum: &Curriculum, lessons: Vec<LessonSummary>) -> CurriculumMatch {
    let mut by_course_and_slug: HashMap<(String, String), usize> = HashMap::new();
    for (index, lesson) in lessons.iter().enumerate() {
        // First wins; the shadowed file stays in `orphans` below.
        by_course_and_slug
            .entry((lesson.course.clone(), lesson.slug.clone()))
            .or_insert(index);
    }

    let mut claimed: HashSet<usize> = HashSet::new();
    let chapters = curriculum
        .chapters
        .iter()
        .map(|chapter| {
            let slug = chapter_slug(chapter);
            let mut taught = 0;
            let rows = planned_lessons(chapter)
                .into_iter()
                .map(|planned| {
                    let lesson = by_course_and_slug
                        .get(&(slug.clone(), planned.slug.clone()))
                        .map(|&index| {
                            claimed.insert(index);
                            lessons[index].clone()
                        });
                    if lesson.is_some() {
                        taught += 1;
                    }
                    MatchedLesson { planned, lesson }
                })
                .collect();
            MatchedChapter {
                title: chapter_title(chapter),
                slug,
                lessons: rows,
                taught,
            }
        })
        .collect();

    let orphans = lessons
        .iter()
        .enumerate()
        .filter(|(index, _)| !claimed.contains(index))
        .map(|(_, lesson)| lesson.clone())
        .collect();
    CurriculumMatch {
        chapters,
        taught: lessons,
        orphans,
    }
}

/// The plan as plain text, for an agent that needs to read it without parsing JSON.
pub fn format_curriculum(m: &CurriculumMatch) -> String {
    let mut blocks: Vec<String> = m
        .chapters
        .iter()
        .map(|chapter| {
            let rows: Vec<String> = chapter
                .lessons
                .iter()
                .map(|row| format_line(&row.planned, row.lesson.as_ref()))
                .collect();
            format!(
                "{} (courses/{}) — {}/{}\n{}",
                chapter.title,
                chapter.slug,
                chapter.taught,
                chapter.lessons.len(),
                rows.join("\n")
            )
        })
        .collect();

    if !m.orphans.is_empty() {
        let rows: Vec<String> = m
            .orphans
            .iter()
            .map(|lesson| {
                let planned = PlannedLesson {
                    title: lesson.title.clone(),
                    slug: lesson.slug.clone(),
                    planned_nodes: None,
                };
                format!(
                    "{}  [courses/{}]",
                    format_line(&planned, Some(lesson)),
                    lesson.course
                )
            })
            .collect();
        blocks.push(format!("Fuori dal piano\n{}", rows.join("\n")));
    }

    if blocks.is_empty() {
        "Nessun capitolo pianificato.".to_string()
    } else {
        blocks.join("\n\n")
    }
}

fn format_line(planned: &PlannedLesson, lesson: Option<&LessonSummary>) -> String {
    let mut line = String::from("  ");
    match lesson {
        Some(lesson) => {
            // JS String(n).padStart(4, "0"); lesson numbers are non-negative in practice.
            let number = if lesson.lesson_number >= 0 {
                format!("{:04}", lesson.lesson_number)
            } else {
                lesson.lesson_number.to_string()
            };
            line.push_str("[x] ");
            line.push_str(&number);
            line.push_str("  ");
            line.push_str(&lesson.title);
            if lesson.node_count == 0 {
                line.push_str(" (prosa)");
            } else if lesson.node_count == 1 {
                line.push_str(" (1 nodo)");
            } else {
                line.push_str(&format!(" ({} nodi)", lesson.node_count));
            }
        }
        None => {
            line.push_str("[ ] ----  ");
            line.push_str(&planned.title);
            if let Some(nodes) = planned.planned_nodes.filter(|nodes| *nodes != 0.0) {
                line.push_str(&format!(" ({} nodi previsti)", js_number(nodes)));
            }
        }
    }
    line
}

fn field_string(map: &Map<String, Value>, key: &str) -> String {
    map.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

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
            "proxi-chapter-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn write(file: &Path, body: &str) {
        fs::create_dir_all(file.parent().expect("parent")).expect("create parent");
        fs::write(file, body).expect("write file");
    }

    fn lessons_json(raw: &str) -> Vec<Value> {
        serde_json::from_str(raw).expect("valid lessons json")
    }

    fn chapter_map(title: &str, slug: &str, lessons: &[(&str, &str)]) -> Map<String, Value> {
        serde_json::json!({
            "title": title,
            "slug": slug,
            "lessons": lessons
                .iter()
                .map(|(title, slug)| serde_json::json!({ "title": title, "slug": slug }))
                .collect::<Vec<_>>(),
        })
        .as_object()
        .expect("chapter object")
        .clone()
    }

    fn summary(
        course: &str,
        lesson_number: i64,
        slug: &str,
        title: &str,
        node_count: usize,
    ) -> LessonSummary {
        LessonSummary {
            course: course.to_string(),
            lesson_number,
            slug: slug.to_string(),
            title: title.to_string(),
            filename: format!("{:04}-{slug}.html", lesson_number),
            node_count,
            tiers: Vec::new(),
            has_reference: false,
        }
    }

    #[test]
    fn missing_curriculum_reads_empty() {
        let t = temp_dir("missing");
        let file = curriculum_file(&t.0, None);
        assert_eq!(file, t.0.join("curriculum.json"));
        assert_eq!(curriculum_file(&t.0, Some("")), t.0.join("curriculum.json"));
        assert_eq!(curriculum_file(&t.0, Some("plan.json")), t.0.join("plan.json"));

        let curriculum = read_curriculum(&file).expect("reads");
        assert!(curriculum.chapters.is_empty());
    }

    #[test]
    fn a_blank_chapter_title_is_refused_before_reading_the_plan() {
        let t = temp_dir("blank-chapter");
        let file = curriculum_file(&t.0, None);
        // Whitespace-only is empty after `js_trim`, and the check runs before any read or write.
        let error = plan_chapter(&file, "   ", None, &lessons_json("[]")).expect_err("blank title");
        assert_eq!(error, "A chapter title is required");
        assert!(!file.exists());
    }

    #[test]
    fn plan_slugifies_titles_and_keeps_an_explicit_course_slug() {
        let t = temp_dir("plan");
        let file = curriculum_file(&t.0, None);
        let curriculum = plan_chapter(
            &file,
            "App State & Caching",
            Some("app-cache"),
            &lessons_json(
                r#"[{"title":"Architetture di cache","plannedNodes":3},{"title":"Probabilità condizionata"}]"#,
            ),
        )
        .expect("plans");

        assert_eq!(curriculum.chapters.len(), 1);
        let chapter = &curriculum.chapters[0];
        assert_eq!(chapter_slug(chapter), "app-cache", "explicit slug wins");
        assert_eq!(chapter_title(chapter), "App State & Caching");
        let planned = planned_lessons(chapter);
        assert_eq!(
            planned.iter().map(|l| l.slug.as_str()).collect::<Vec<_>>(),
            ["architetture-di-cache", "probabilita-condizionata"]
        );
        assert_eq!(planned[0].planned_nodes, Some(3.0));
        assert_eq!(read_curriculum(&file).expect("reads"), curriculum);
    }

    #[test]
    fn plan_keeps_an_explicit_lesson_slug() {
        let t = temp_dir("lesson-slug");
        let file = curriculum_file(&t.0, None);
        let curriculum = plan_chapter(
            &file,
            "Docker",
            None,
            &lessons_json(
                r#"[{"title":"Image e container: la distinzione che regge tutto","slug":"image-e-container"}]"#,
            ),
        )
        .expect("plans");

        assert_eq!(
            planned_lessons(&curriculum.chapters[0])[0].slug,
            "image-e-container"
        );
    }

    #[test]
    fn replan_replaces_lessons_and_keeps_the_other_chapters() {
        let t = temp_dir("replan");
        let file = curriculum_file(&t.0, None);
        plan_chapter(&file, "Docker", None, &lessons_json(r#"[{"title":"Image"}]"#)).unwrap();
        plan_chapter(&file, "Nginx", None, &lessons_json(r#"[{"title":"Proxy"}]"#)).unwrap();

        let curriculum = plan_chapter(
            &file,
            "Docker",
            None,
            &lessons_json(r#"[{"title":"Image"},{"title":"Compose"}]"#),
        )
        .unwrap();

        assert_eq!(
            curriculum
                .chapters
                .iter()
                .map(chapter_slug)
                .collect::<Vec<_>>(),
            ["docker", "nginx"]
        );
        assert_eq!(
            planned_lessons(&curriculum.chapters[0])
                .iter()
                .map(|l| l.slug.as_str())
                .collect::<Vec<_>>(),
            ["image", "compose"]
        );
        assert_eq!(
            planned_lessons(&curriculum.chapters[1])
                .iter()
                .map(|l| l.slug.as_str())
                .collect::<Vec<_>>(),
            ["proxy"]
        );
    }

    #[test]
    fn duplicate_slugs_are_refused_with_the_full_text() {
        let t = temp_dir("duplicate");
        let file = curriculum_file(&t.0, None);
        let err = plan_chapter(
            &file,
            "Docker",
            None,
            &lessons_json(r#"[{"title":"Image e container"},{"title":"Image, e container!"}]"#),
        )
        .unwrap_err();

        assert_eq!(
            err,
            "Two planned lessons share the slug \"image-e-container\": \"Image e container\" and \"Image, e container!\". Give them distinct titles, or pass an explicit slug."
        );
    }

    #[test]
    fn malformed_chapters_are_refused_and_the_file_is_left_unchanged() {
        let t = temp_dir("malformed");
        let file = curriculum_file(&t.0, None);
        let body = r#"{"chapters":{"docker":{}}}"#;
        write(&file, body);

        let err = plan_chapter(&file, "New", None, &lessons_json(r#"[{"title":"A"}]"#))
            .expect_err("refuses");
        assert!(err.contains("\"chapters\" must be an array"), "{err}");
        assert_eq!(fs::read_to_string(&file).unwrap(), body);
    }

    #[test]
    fn a_chapter_without_lessons_reads_as_empty() {
        let t = temp_dir("no-lessons");
        let file = curriculum_file(&t.0, None);
        write(&file, r#"{"chapters":[{"title":"X","slug":"x"}]}"#);

        let curriculum = read_curriculum(&file).unwrap();
        let m = match_curriculum(&curriculum, Vec::new());
        assert!(m.chapters[0].lessons.is_empty());
        assert_eq!(m.chapters[0].taught, 0);
    }

    #[test]
    fn unknown_chapter_keys_and_position_survive_planning_another_chapter() {
        let t = temp_dir("unknown-keys");
        let file = curriculum_file(&t.0, None);
        write(
            &file,
            r#"{
  "chapters": [
    {
      "title": "Docker",
      "slug": "docker",
      "note": "tenuto",
      "lessons": [
        {
          "title": "Image",
          "slug": "image"
        }
      ]
    },
    {
      "title": "Nginx",
      "slug": "nginx",
      "lessons": []
    }
  ]
}"#,
        );

        plan_chapter(&file, "Rust", None, &lessons_json(r#"[{"title":"Ownership"}]"#)).unwrap();

        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            r#"{
  "chapters": [
    {
      "title": "Docker",
      "slug": "docker",
      "note": "tenuto",
      "lessons": [
        {
          "title": "Image",
          "slug": "image"
        }
      ]
    },
    {
      "title": "Nginx",
      "slug": "nginx",
      "lessons": []
    },
    {
      "title": "Rust",
      "slug": "rust",
      "lessons": [
        {
          "title": "Ownership",
          "slug": "ownership"
        }
      ]
    }
  ]
}
"#
        );
    }

    #[test]
    fn written_files_are_two_space_indented_and_end_with_a_newline() {
        let t = temp_dir("pretty");
        let file = curriculum_file(&t.0, None);
        plan_chapter(&file, "Docker", None, &lessons_json(r#"[{"title":"Image"}]"#)).unwrap();

        let text = fs::read_to_string(&file).unwrap();
        assert!(text.ends_with('\n'));
        assert!(text.contains("\n  \"chapters\": [\n"), "{text}");
        assert!(
            text.contains("\n    {\n      \"title\": \"Docker\",\n"),
            "{text}"
        );
    }

    #[test]
    fn match_does_not_let_one_course_claim_another_same_slug_entry() {
        let curriculum = Curriculum {
            chapters: vec![
                chapter_map("Docker", "docker", &[("Basi", "basi")]),
                chapter_map("Nginx", "nginx", &[("Basi", "basi")]),
            ],
        };
        let m = match_curriculum(
            &curriculum,
            vec![summary("docker", 1, "basi", "Basi", 0)],
        );

        assert_eq!(m.chapters[0].taught, 1);
        assert_eq!(m.chapters[1].taught, 0, "nginx/basi is still to do");
        assert!(m.orphans.is_empty());
    }

    #[test]
    fn a_slug_shadowed_second_lesson_stays_in_orphans() {
        let curriculum = Curriculum {
            chapters: vec![chapter_map(
                "Docker",
                "docker",
                &[("Compose", "compose")],
            )],
        };
        let m = match_curriculum(
            &curriculum,
            vec![
                summary("docker", 1, "compose", "Compose, prima versione", 0),
                summary("docker", 2, "compose", "Compose, seconda versione", 0),
            ],
        );

        assert_eq!(m.chapters[0].taught, 1);
        // The one the plan did not claim must still surface somewhere, not vanish.
        assert_eq!(m.orphans.len(), 1);
        assert_eq!(m.orphans[0].lesson_number, 2);
    }

    #[test]
    fn format_curriculum_prints_the_plan_the_way_agents_read_it() {
        let m = CurriculumMatch {
            chapters: vec![
                MatchedChapter {
                    title: "Docker".to_string(),
                    slug: "docker".to_string(),
                    taught: 3,
                    lessons: vec![
                        MatchedLesson {
                            planned: PlannedLesson {
                                title: "Image e container".to_string(),
                                slug: "image-e-container".to_string(),
                                planned_nodes: None,
                            },
                            lesson: Some(summary(
                                "docker",
                                7,
                                "image-e-container",
                                "Image e container",
                                4,
                            )),
                        },
                        MatchedLesson {
                            planned: PlannedLesson {
                                title: "Healthcheck".to_string(),
                                slug: "healthcheck".to_string(),
                                planned_nodes: Some(4.0),
                            },
                            lesson: None,
                        },
                        MatchedLesson {
                            planned: PlannedLesson {
                                title: "Prosa".to_string(),
                                slug: "prosa".to_string(),
                                planned_nodes: None,
                            },
                            lesson: Some(summary("docker", 8, "prosa", "Prosa", 0)),
                        },
                        MatchedLesson {
                            planned: PlannedLesson {
                                title: "Solo".to_string(),
                                slug: "solo".to_string(),
                                planned_nodes: None,
                            },
                            lesson: Some(summary("docker", 9, "solo", "Solo", 1)),
                        },
                    ],
                },
                MatchedChapter {
                    title: "Nginx".to_string(),
                    slug: "nginx".to_string(),
                    taught: 0,
                    lessons: vec![MatchedLesson {
                        planned: PlannedLesson {
                            title: "Proxy".to_string(),
                            slug: "proxy".to_string(),
                            planned_nodes: None,
                        },
                        lesson: None,
                    }],
                },
            ],
            taught: Vec::new(),
            orphans: vec![summary("rust", 2, "ownership", "Ownership", 2)],
        };

        assert_eq!(
            format_curriculum(&m),
            r#"Docker (courses/docker) — 3/4
  [x] 0007  Image e container (4 nodi)
  [ ] ----  Healthcheck (4 nodi previsti)
  [x] 0008  Prosa (prosa)
  [x] 0009  Solo (1 nodo)

Nginx (courses/nginx) — 0/1
  [ ] ----  Proxy

Fuori dal piano
  [x] 0002  Ownership (2 nodi)  [courses/rust]"#
        );
    }

    #[test]
    fn format_curriculum_of_an_empty_match_says_there_is_no_plan() {
        let m = CurriculumMatch {
            chapters: Vec::new(),
            taught: Vec::new(),
            orphans: Vec::new(),
        };
        assert_eq!(format_curriculum(&m), "Nessun capitolo pianificato.");
    }
}
