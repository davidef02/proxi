//! Replays the Node prompt golden captured by app/test/capture-parity-golden.ts:
//! every case builds the same `PromptInput` on a fresh fixture and compares
//! prompt and opening byte-for-byte with the frozen expectation.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use common::cli_golden;
use proxi::context::{build_system_prompt, opening_message, PromptInput};

const TITLE: &str = "Il titolo pianificato della lezione due";
const LESSON: &str = "courses/demo/lessons/0001-lezione-uno.html";
const DAG: &str = "graph TD\n  N1[\"Nodo 1: Prima cosa\"] --> N2[\"Nodo 2: Seconda cosa\"]\n  N2 --> N3[\"Nodo 3: Terza cosa\"]";

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
    repo: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let repo_dir = common::temp_dir("ctx-parity-repo");
        let data_dir = common::temp_dir("ctx-parity-data");
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
            repo,
            data,
        }
    }
}

/// The capture's normalisation: temp roots back to their tags, then separators and CRLF.
fn normalise(text: &str, roots: &[(String, String)]) -> String {
    let mut out = text.to_string();
    for (root, tag) in roots {
        out = out.replace(root.as_str(), tag.as_str());
    }
    let out = out.replace("\\\\", "/").replace('\\', "/");
    out.replace("\r\n", "\n")
}

#[test]
fn system_prompt_replays_the_node_golden_for_every_session_kind() {
    let golden = cli_golden::golden("context");
    let cases = golden["cases"].as_array().expect("cases array");
    let names: Vec<&str> = cases
        .iter()
        .map(|case| case["name"].as_str().expect("case name"))
        .collect();
    assert_eq!(
        names,
        vec![
            "teach-planned-anki",
            "teach-planned-no-anki",
            "teach-existing",
            "teach-existing-target-path",
            "teach-free-standalone",
            "plan-existing",
            "plan-new",
            "practice-no-pick",
            "practice-nodes",
            "practice-unknown-nodes",
            "practice-propositions",
            "teach-planned-repo-as-data",
        ],
        "golden case names"
    );

    let fixture = Fixture::new();
    let code_root = common::repo_root();
    let roots = vec![
        (fixture.data.display().to_string(), "<dataRoot>".to_string()),
        (fixture.repo.display().to_string(), "<repo>".to_string()),
        (code_root.display().to_string(), "<repoRoot>".to_string()),
    ];

    for case in cases {
        let name = case["name"].as_str().expect("case name");
        let input = &case["input"];
        let config = input["config"].as_object().expect("config object");
        let data_root = input.get("dataRoot").map(|_| fixture.data.as_path());
        let lesson_path = input.get("lessonPath").and_then(Value::as_str);
        let today = input["today"].as_str().expect("today");
        let lesson_title = input.get("lessonTitle").and_then(Value::as_str);

        let prompt = build_system_prompt(&PromptInput {
            config,
            code_root: &code_root,
            repo_root: &fixture.repo,
            data_root,
            lesson_path,
            today,
        });
        let opening = opening_message(config, lesson_title);

        let rust_prompt = normalise(&prompt, &roots);
        let rust_opening = normalise(&opening, &roots);

        // A fixture read as empty would compare equal and prove nothing.
        assert!(
            rust_prompt.contains("# Course index") && rust_prompt.contains("# This session"),
            "{name}: fixture not read:\n{rust_prompt}"
        );
        for marker in case["markers"].as_array().expect("markers") {
            let marker = marker.as_str().expect("marker");
            assert!(
                rust_prompt.contains(marker),
                "{name}: prompt lacks {marker:?}:\n{rust_prompt}"
            );
        }

        assert_eq!(
            rust_prompt,
            case["prompt"].as_str().expect("golden prompt"),
            "{name}: system prompt differs"
        );
        assert_eq!(
            rust_opening,
            case["opening"].as_str().expect("golden opening"),
            "{name}: opening message differs"
        );
    }
}
