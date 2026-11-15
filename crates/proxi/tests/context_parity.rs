//! Live Node ↔ Rust parity for the system prompt: every session kind must come out byte-identical.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use proxi::context::{build_system_prompt, opening_message, PromptInput};

const TITLE: &str = "Il titolo pianificato della lezione due";
const LESSON: &str = "courses/demo/lessons/0001-lezione-uno.html";
const DAG: &str = "graph TD\n  N1[\"Nodo 1: Prima cosa\"] --> N2[\"Nodo 2: Seconda cosa\"]\n  N2 --> N3[\"Nodo 3: Terza cosa\"]";
const TODAY: &str = "2026-10-27";

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
}

fn lesson_island(doc: Value) -> String {
    format!("<script type=\"application/json\" id=\"lesson-data\">{doc}</script>")
}

/// One temp code repo (reference docs) and a separate temp data folder with a full course.
struct Fixture {
    _repo_dir: common::TempDir,
    _data_dir: common::TempDir,
    _input_dir: common::TempDir,
    repo: PathBuf,
    data: PathBuf,
    inputs: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let repo_dir = common::temp_dir("ctx-parity-repo");
        let data_dir = common::temp_dir("ctx-parity-data");
        let input_dir = common::temp_dir("ctx-parity-input");
        let repo = repo_dir.0.clone();
        let data = data_dir.0.clone();

        let real = common::repo_root();
        for name in ["pedagogy.md", "anki-cards.md"] {
            let text = fs::read_to_string(real.join("docs").join("agents").join(name)).unwrap();
            write(&repo.join("docs").join("agents").join(name), &text);
        }

        write(
            &data.join("curriculum.json"),
            &json!({
                "chapters": [
                    {
                        "title": "Demo",
                        "slug": "demo",
                        "lessons": [
                            { "title": "Lezione uno", "slug": "lezione-uno", "plannedNodes": 3 },
                            { "title": TITLE, "slug": "il-titolo-pianificato-della-lezione-due", "plannedNodes": 4 },
                        ],
                    },
                    {
                        "title": "Altro",
                        "slug": "altro",
                        "lessons": [
                            { "title": "Lezione tre", "slug": "lezione-tre", "plannedNodes": 2 },
                        ],
                    },
                ],
            })
            .to_string(),
        );

        write(&data.join("courses/demo/MISSION.md"), "# Mission\nMISSION-MARKER");
        write(&data.join("courses/demo/NOTES.md"), "# Notes\nNOTES-MARKER");
        write(
            &data.join("courses/demo/GLOSSARY.md"),
            "# Glossario\nGLOSSARY-MARKER",
        );
        for n in 1..=4 {
            write(
                &data.join(format!("courses/demo/learning-records/000{n}-x{n}.md")),
                &format!("# rec {n}\nREC-{n}"),
            );
        }
        write(&data.join("courses/demo/exercises/package.json"), "{}");
        fs::create_dir_all(data.join("courses/demo/exercises/01-a")).unwrap();
        fs::create_dir_all(data.join("courses/demo/exercises/zz-b")).unwrap();

        let demo = json!({
            "metadata": {
                "lessonNumber": 1,
                "topic": "Demo topic",
                "title": "Lezione uno",
                "slug": "lezione-uno",
                "dateCreated": "2026-09-01T08:00:00.000Z",
                "dateModified": "2026-09-01T08:00:00.000Z",
                "initialDag": DAG,
                "targetGoal": "Capire la prima cosa",
                "version": "1.0.0"
            },
            "nodes": [{
                "nodeId": "prima-cosa",
                "title": "Prima cosa",
                "tier": "unconditional-truth",
                "content": "<p>x</p>"
            }]
        });
        write(&data.join(LESSON), &lesson_island(demo));
        let demo_progress = json!({
            "at": "2026-09-01T08:00:00.000Z",
            "sessionId": "s1",
            "course": "demo",
            "lessonSlug": "lezione-uno",
            "nodeRefs": ["demo/lezione-uno#prima-cosa"],
            "kind": "review",
            "source": "anki",
            "reviewId": 1,
            "cardId": 1,
            "ease": 1,
            "lapses": 2,
            "interval": 0
        });
        write(
            &data.join("courses/demo/progress.jsonl"),
            &format!("{demo_progress}\n"),
        );

        let orphan = json!({
            "metadata": {
                "lessonNumber": 1,
                "topic": "Tema orfano",
                "title": "Lezione orfana",
                "slug": "lezione-orfana",
                "dateCreated": "2026-09-02T08:00:00.000Z",
                "dateModified": "2026-09-02T08:00:00.000Z",
                "initialDag": "graph TD\n  N1[\"Nodo 1: Nodo orfano\"] --> N2[\"Nodo 2: Coda orfana\"]",
                "targetGoal": "Capire l'orfano",
                "version": "1.0.0"
            },
            "nodes": [{
                "nodeId": "nodo-orfano",
                "title": "Nodo orfano",
                "tier": "axiom",
                "content": "<p>y</p>"
            }]
        });
        write(
            &data.join("courses/orfano/lessons/0001-lezione-orfana.html"),
            &lesson_island(orphan),
        );
        let orphan_progress = json!({
            "at": "2026-09-02T08:00:00.000Z",
            "sessionId": "s3",
            "course": "orfano",
            "lessonSlug": "lezione-orfana",
            "nodeRefs": ["orfano/lezione-orfana#nodo-orfano"],
            "kind": "check",
            "correct": true,
            "confidence": 80
        });
        write(
            &data.join("courses/orfano/progress.jsonl"),
            &format!("{orphan_progress}\n"),
        );

        common::write_node_lines(
            &data,
            &[
                (
                    "n_prima",
                    "Concetto primo",
                    "La prima proposizione del grafo.",
                ),
                (
                    "n_seconda",
                    "Concetto secondo",
                    "La seconda proposizione del grafo.",
                ),
            ],
        );

        Fixture {
            _repo_dir: repo_dir,
            _data_dir: data_dir,
            inputs: input_dir.0.clone(),
            _input_dir: input_dir,
            repo,
            data,
        }
    }
}

struct Case {
    name: &'static str,
    config: Value,
    data_root: bool,
    lesson_path: Option<&'static str>,
    lesson_title: Option<&'static str>,
    /// Markers that prove Node read the fixture, so an empty read cannot pass silently.
    markers: &'static [&'static str],
}

fn cases() -> Vec<Case> {
    let teach_planned = |anki: bool, model: &str| {
        json!({
            "kind": "teach",
            "course": "demo",
            "model": model,
            "target": { "mode": "planned", "title": TITLE },
            "ankiOptIn": anki,
        })
    };
    let teach_existing = || {
        json!({
            "kind": "teach",
            "course": "demo",
            "model": "sonnet",
            "target": { "mode": "existing", "lessonPath": LESSON },
            "ankiOptIn": false,
        })
    };
    let practice = |extra: Value| {
        let mut config = json!({
            "kind": "practice",
            "course": "demo",
            "model": "haiku",
            "practiceNodes": [],
        });
        if let (Some(config), Some(extra)) = (config.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                config.insert(key.clone(), value.clone());
            }
        }
        config
    };

    vec![
        Case {
            name: "teach-planned-anki",
            config: teach_planned(true, "sonnet"),
            data_root: true,
            lesson_path: None,
            lesson_title: None,
            markers: &["Anki opt-in: Sì", TITLE, "REC-2", "anki-cards.md"],
        },
        Case {
            name: "teach-planned-no-anki",
            config: teach_planned(false, "haiku"),
            data_root: true,
            lesson_path: None,
            lesson_title: None,
            markers: &["Anki opt-in: No", "<chapter-plan>"],
        },
        Case {
            name: "teach-existing",
            config: teach_existing(),
            data_root: true,
            lesson_path: Some(LESSON),
            lesson_title: Some("Lezione uno"),
            markers: &["Captured nodes (1 of 3 planned)", "Capire la prima cosa"],
        },
        Case {
            name: "teach-existing-target-path",
            config: teach_existing(),
            data_root: true,
            lesson_path: None,
            lesson_title: Some("Lezione uno"),
            markers: &["Captured nodes (1 of 3 planned)", "Capire la prima cosa"],
        },
        Case {
            name: "teach-free-standalone",
            config: json!({
                "kind": "teach",
                "course": "lezioni-singole",
                "model": "sonnet",
                "target": { "mode": "free" },
                "ankiOptIn": false,
                "note": "  Voglio partire dalle basi.  ",
            }),
            data_root: true,
            lesson_path: None,
            lesson_title: None,
            markers: &["Learner's note: Voglio partire dalle basi."],
        },
        Case {
            name: "plan-existing",
            config: json!({
                "kind": "plan",
                "course": "demo",
                "model": "sonnet",
                "chapterTitle": "Demo",
                "note": "Partiamo dal piano.",
            }),
            data_root: true,
            lesson_path: None,
            lesson_title: None,
            markers: &[
                "<chapter-plan>",
                "GLOSSARY-MARKER",
                "<exercises course=\"demo\">",
                "01-a",
                "zz-b",
            ],
        },
        Case {
            name: "plan-new",
            config: json!({
                "kind": "plan",
                "course": "nuovo",
                "model": "opus",
                "chapterTitle": "Nuovo corso",
            }),
            data_root: true,
            lesson_path: None,
            lesson_title: None,
            markers: &["is new: provide a full mission"],
        },
        Case {
            name: "practice-no-pick",
            config: practice(json!({})),
            data_root: true,
            lesson_path: None,
            lesson_title: None,
            markers: &["- key \"demo/lezione-uno#prima-cosa\""],
        },
        Case {
            name: "practice-nodes",
            config: practice(json!({ "practiceNodes": ["demo/lezione-uno#prima-cosa"] })),
            data_root: true,
            lesson_path: None,
            lesson_title: None,
            markers: &["Selected nodes to practise"],
        },
        Case {
            name: "practice-unknown-nodes",
            config: practice(json!({ "practiceNodes": ["demo/lezione-uno#sparito"] })),
            data_root: true,
            lesson_path: None,
            lesson_title: None,
            markers: &["not found in the map"],
        },
        Case {
            name: "practice-propositions",
            config: practice(json!({ "practicePropositions": ["n_prima", "n_mancante"] })),
            data_root: true,
            lesson_path: None,
            lesson_title: None,
            markers: &[
                "Concetto primo",
                "La prima proposizione del grafo.",
                "n_mancante (not found in the map)",
            ],
        },
        Case {
            name: "teach-planned-repo-as-data",
            config: teach_planned(false, "sonnet"),
            data_root: false,
            lesson_path: None,
            lesson_title: None,
            markers: &["The knowledge graph is empty"],
        },
    ]
}

fn run(fixture: &Fixture, case: &Case) {
    let config = case.config.as_object().expect("config object").clone();
    let mut input = Map::new();
    input.insert("config".to_string(), Value::Object(config.clone()));
    input.insert(
        "repoRoot".to_string(),
        Value::String(fixture.repo.display().to_string()),
    );
    if case.data_root {
        input.insert(
            "dataRoot".to_string(),
            Value::String(fixture.data.display().to_string()),
        );
    }
    if let Some(path) = case.lesson_path {
        input.insert("lessonPath".to_string(), Value::String(path.to_string()));
    }
    input.insert("today".to_string(), Value::String(TODAY.to_string()));
    if let Some(title) = case.lesson_title {
        input.insert("lessonTitle".to_string(), Value::String(title.to_string()));
    }

    let file = fixture.inputs.join(format!("{}.json", case.name));
    write(&file, &Value::Object(input).to_string());
    let file_arg = file.display().to_string();
    let output = common::node(&["app/test/prompt-dump.ts", &file_arg])
        .output()
        .unwrap_or_else(|error| panic!("{}: run prompt-dump: {error}", case.name));
    assert!(
        output.status.success(),
        "{}: prompt-dump failed:\n{}",
        case.name,
        String::from_utf8_lossy(&output.stderr)
    );
    let node: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{}: prompt-dump printed invalid JSON ({error}): {}",
            case.name,
            String::from_utf8_lossy(&output.stdout)
        )
    });
    let node_prompt = node
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{}: no prompt string: {node}", case.name));
    let node_opening = node
        .get("opening")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{}: no opening string: {node}", case.name));
    // A fixture both sides read as empty would compare equal and prove nothing.
    assert!(
        node_prompt.contains("# Course index") && node_prompt.contains("# This session"),
        "{}: Node did not read the fixture:\n{node_prompt}",
        case.name
    );
    for marker in case.markers {
        assert!(
            node_prompt.contains(marker),
            "{}: Node prompt lacks {marker:?}:\n{node_prompt}",
            case.name
        );
    }

    let code_root = common::repo_root();
    let data_root = case.data_root.then_some(fixture.data.as_path());
    let rust_prompt = build_system_prompt(&PromptInput {
        config: &config,
        code_root: &code_root,
        repo_root: &fixture.repo,
        data_root,
        lesson_path: case.lesson_path,
        today: TODAY,
    });
    let rust_opening = opening_message(&config, case.lesson_title);

    assert_eq!(node_prompt, rust_prompt, "{}: system prompt differs", case.name);
    assert_eq!(
        node_opening, rust_opening,
        "{}: opening message differs",
        case.name
    );
}

#[test]
fn system_prompt_matches_node_for_every_session_kind() {
    common::require_node();
    let fixture = Fixture::new();
    for case in cases() {
        run(&fixture, &case);
    }
}
