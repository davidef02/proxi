//! Job result delivery text (port of app/server/jobs/deliver.ts).

use serde_json::Value;

/// The learner-side turn that hands a finished (or failed) job to the teacher.
pub fn job_turn_text(job: &Value) -> String {
    let text = |key: &str| job.get(key).and_then(Value::as_str).unwrap_or_default();
    let head = format!(
        "[Risultato del subagente {} · job {}]",
        text("kind"),
        text("id")
    );
    let result = job
        .get("result")
        .filter(|value| crate::js::truthy(Some(value)));
    if text("status") != "done" || result.is_none() {
        let error = job
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("errore sconosciuto");
        return format!("{head}\n\nIl subagente non ha consegnato: {error}.");
    }
    let field = |key: &str| result.and_then(|r| r.get(key)).and_then(Value::as_str);
    let optional = |label: &str, key: &str| match field(key).filter(|value| !value.is_empty()) {
        Some(value) => format!("\n{label}: {value}"),
        None => String::new(),
    };
    match field("kind").unwrap_or_default() {
        "research" => {
            let sources = result
                .and_then(|r| r.get("sources"))
                .and_then(Value::as_array)
                .map(|sources| {
                    sources
                        .iter()
                        .map(|source| {
                            format!(
                                "- {}: {}",
                                source
                                    .get("title")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default(),
                                source
                                    .get("url")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            format!(
                "{head}\n\n{}\n\nFonti:\n{sources}",
                field("brief").unwrap_or_default()
            )
        }
        "visual" => format!(
            "{head}\n\nLa figura è già visibile allo studente nella chat.{}",
            optional("Didascalia", "caption")
        ),
        "demo" => format!(
            "{head}\n\nLa demo \"{}\" è già visibile allo studente nella chat.{}",
            field("title").unwrap_or_default(),
            optional("Note", "notes")
        ),
        // Map jobs are never delivered.
        _ => head,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn job(kind: &str, status: &str) -> Value {
        json!({
            "id": "j1",
            "kind": kind,
            "title": "t",
            "model": "haiku",
            "activity": "",
            "createdAt": "",
            "updatedAt": "",
            "costUsd": 0,
            "status": status,
        })
    }

    #[test]
    fn research_carries_the_brief_and_the_sources() {
        let mut record = job("research", "done");
        record["result"] = json!({
            "kind": "research",
            "brief": "## Sintesi\nx",
            "sources": [{"title": "A", "url": "https://a"}, {"title": "B", "url": "https://b"}],
        });
        assert_eq!(
            job_turn_text(&record),
            "[Risultato del subagente research · job j1]\n\n## Sintesi\nx\n\nFonti:\n- A: https://a\n- B: https://b"
        );
    }

    #[test]
    fn visual_and_demo_carry_their_optional_caption_and_notes() {
        let mut visual = job("visual", "done");
        visual["result"] = json!({"kind": "visual", "svg": "<svg/>", "caption": "Cap"});
        assert_eq!(
            job_turn_text(&visual),
            "[Risultato del subagente visual · job j1]\n\nLa figura è già visibile allo studente nella chat.\nDidascalia: Cap"
        );
        visual["result"] = json!({"kind": "visual", "svg": "<svg/>"});
        assert_eq!(
            job_turn_text(&visual),
            "[Risultato del subagente visual · job j1]\n\nLa figura è già visibile allo studente nella chat."
        );

        let mut demo = job("demo", "done");
        demo["result"] = json!({"kind": "demo", "title": "T", "html": "x", "notes": "N"});
        assert_eq!(
            job_turn_text(&demo),
            "[Risultato del subagente demo · job j1]\n\nLa demo \"T\" è già visibile allo studente nella chat.\nNote: N"
        );
        demo["result"] = json!({"kind": "demo", "title": "T", "html": "x"});
        assert_eq!(
            job_turn_text(&demo),
            "[Risultato del subagente demo · job j1]\n\nLa demo \"T\" è già visibile allo studente nella chat."
        );
    }

    #[test]
    fn a_failed_job_names_its_error_and_a_missing_one_the_default() {
        let mut record = job("research", "error");
        record["error"] = json!("boom");
        assert_eq!(
            job_turn_text(&record),
            "[Risultato del subagente research · job j1]\n\nIl subagente non ha consegnato: boom."
        );
        record.as_object_mut().unwrap().remove("error");
        assert_eq!(
            job_turn_text(&record),
            "[Risultato del subagente research · job j1]\n\nIl subagente non ha consegnato: errore sconosciuto."
        );

        // A done job that never handed a result is the same as a failed one.
        let pending = job("research", "running");
        assert_eq!(
            job_turn_text(&pending),
            "[Risultato del subagente research · job j1]\n\nIl subagente non ha consegnato: errore sconosciuto."
        );
    }

    #[test]
    fn a_map_job_is_only_its_head() {
        let mut record = job("map", "done");
        record["result"] =
            json!({"kind": "map", "source": "c/l", "added": 1, "linked": 0, "chains": 0});
        assert_eq!(
            job_turn_text(&record),
            "[Risultato del subagente map · job j1]"
        );
    }
}
