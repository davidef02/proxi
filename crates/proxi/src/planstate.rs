//! Plan DAG validation and node status derivation (port of `app/server/planstate.ts`).

use serde_json::{Map, Value};

use crate::js;

pub use crate::lesson::parse_dag_ids;

/// Errors (line-numbered where possible); empty when the DAG is acceptable.
pub fn validate_dag_labels(dag: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let lines: Vec<&str> = dag
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let first = lines.iter().position(|line| !js::js_trim(line).is_empty());
    if !first.is_some_and(|i| has_header(js::js_trim(lines[i]))) {
        let line = first.map_or(1, |i| i + 1);
        errors.push(format!(
            "line {line}: the DAG must start with \"graph TD\", \"graph LR\", \"flowchart TD\" or \"flowchart LR\"."
        ));
    }
    let mut found = 0;
    for (i, line) in lines.iter().enumerate() {
        for (number, label) in dag_labels(line) {
            found += 1;
            let prefix = format!("\"Nodo {number}: ");
            if !js::js_trim(&label).starts_with(&prefix) {
                errors.push(format!(
                    "line {}: label of N{number} must be quoted and start with \"Nodo {number}: \", e.g. N{number}[\"Nodo {number}: ...\"].",
                    i + 1
                ));
            }
        }
    }
    if found == 0 {
        errors.push(
            "no plan node found: define nodes as N1[\"Nodo 1: ...\"], N2[\"Nodo 2: ...\"], ..."
                .to_string(),
        );
    }
    errors
}

/// `^(graph|flowchart)\s+(TD|LR)\b` on one already-trimmed line.
fn has_header(line: &str) -> bool {
    let rest = line
        .strip_prefix("graph")
        .or_else(|| line.strip_prefix("flowchart"));
    let Some(rest) = rest else {
        return false;
    };
    let mut chars = rest.chars();
    let Some(c) = chars.next() else {
        return false;
    };
    if !js::is_js_ws(c) {
        return false;
    }
    let rest = rest.trim_start_matches(js::is_js_ws);
    for direction in ["TD", "LR"] {
        if let Some(after) = rest.strip_prefix(direction) {
            // `\b`: end of line, or a char JS does not count as a word char.
            return after
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
        }
    }
    false
}

/// `\bN(\d+)\s*\[([^\]]*)\]` matches in order: (digits, label).
fn dag_labels(line: &str) -> Vec<(String, String)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'N' && (i == 0 || !js::is_word_byte(bytes[i - 1])) {
            let mut digits_end = i + 1;
            while digits_end < bytes.len() && bytes[digits_end].is_ascii_digit() {
                digits_end += 1;
            }
            if digits_end > i + 1 {
                let mut after_ws = digits_end;
                while let Some(c) = line[after_ws..].chars().next() {
                    if js::is_js_ws(c) {
                        after_ws += c.len_utf8();
                    } else {
                        break;
                    }
                }
                if after_ws < bytes.len() && bytes[after_ws] == b'[' {
                    if let Some(close) = line[after_ws + 1..].find(']') {
                        let close = after_ws + 1 + close;
                        out.push((
                            line[i + 1..digits_end].to_string(),
                            line[after_ws + 1..close].to_string(),
                        ));
                        i = close + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

/// Status per DAG id. Lesson node i (capture order) realises N(i+1) => done. A latest incorrect
/// `check` (or dontKnow) overrides done with missed. `current` = node of the latest predict/check
/// record when still todo, else the first todo.
pub fn derive_plan_state(
    topic: &str,
    goal: &str,
    dag: &str,
    approved: bool,
    lesson_node_count: usize,
    progress: &[Value],
) -> Value {
    let ids = parse_dag_ids(dag);
    let mut node_status = Map::new();
    for (i, id) in ids.iter().enumerate() {
        let status = if i < lesson_node_count { "done" } else { "todo" };
        node_status.insert(id.clone(), Value::String(status.into()));
    }
    for id in &ids {
        let latest = progress
            .iter()
            .rev()
            .find(|r| kind(r) == Some("check") && refs_include(r, id));
        if latest.is_some_and(|r| r.get("correct") == Some(&Value::Bool(false))) {
            node_status.insert(id.clone(), Value::String("missed".into()));
        }
    }
    let last = progress.iter().rev().find(|r| {
        matches!(kind(r), Some("predict" | "check"))
            && refs(r).iter().any(|n| ids.iter().any(|id| id == n))
    });
    let last_id = last.and_then(|r| {
        refs(r)
            .iter()
            .filter_map(Value::as_str)
            .find(|n| ids.iter().any(|id| id == n))
    });
    let current = match last_id {
        Some(id) if node_status.get(id).and_then(Value::as_str) == Some("todo") => {
            Some(id.to_string())
        }
        _ => ids
            .iter()
            .find(|id| node_status.get(*id).and_then(Value::as_str) == Some("todo"))
            .cloned(),
    };
    if let Some(current) = current {
        node_status.insert(current, Value::String("current".into()));
    }

    // Same key order as Node's object literal.
    let mut out = Map::new();
    out.insert("topic".into(), Value::String(topic.to_string()));
    out.insert("goal".into(), Value::String(goal.to_string()));
    out.insert("dag".into(), Value::String(dag.to_string()));
    out.insert("approved".into(), Value::Bool(approved));
    out.insert("nodeStatus".into(), Value::Object(node_status));
    Value::Object(out)
}

fn kind(record: &Value) -> Option<&str> {
    record.get("kind").and_then(Value::as_str)
}

fn refs(record: &Value) -> &[Value] {
    match record.get("nodeRefs") {
        Some(Value::Array(items)) => items,
        _ => &[],
    }
}

fn refs_include(record: &Value, id: &str) -> bool {
    refs(record).iter().any(|n| n.as_str() == Some(id))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const DAG: &str = "graph TD\n  N1[\"Nodo 1: la prima cosa è vera\"] --> N2[\"Nodo 2: la seconda segue\"]\n  N2 --> N3[\"Nodo 3: il traguardo\"]\n  N1 --> N3";

    fn rec(node_refs: &[&str], kind: &str, correct: Option<bool>) -> Value {
        let mut record = serde_json::Map::new();
        record.insert("at".into(), json!(""));
        record.insert("sessionId".into(), json!("s"));
        record.insert("course".into(), json!("c"));
        record.insert(
            "nodeRefs".into(),
            json!(node_refs.iter().map(|n| n.to_string()).collect::<Vec<_>>()),
        );
        record.insert("kind".into(), json!(kind));
        if let Some(correct) = correct {
            record.insert("correct".into(), json!(correct));
        }
        Value::Object(record)
    }

    fn statuses(state: &Value) -> Map<String, Value> {
        state["nodeStatus"].as_object().expect("nodeStatus").clone()
    }

    fn status_list(state: &Value) -> Vec<(String, String)> {
        statuses(state)
            .into_iter()
            .map(|(id, status)| (id, status.as_str().unwrap_or("").to_string()))
            .collect()
    }

    #[test]
    fn parse_dag_ids_returns_ids_in_first_appearance_order_deduplicated() {
        assert_eq!(parse_dag_ids(DAG), vec!["N1", "N2", "N3"]);
    }

    #[test]
    fn parse_dag_ids_ignores_ids_never_defined_with_a_label_and_other_prefixes() {
        assert_eq!(
            parse_dag_ids("graph TD\n L1[\"x\"] --> N2[\"Nodo 2: y\"]\n N9 --> N2"),
            vec!["N2"]
        );
    }

    #[test]
    fn validate_accepts_a_well_formed_dag() {
        assert_eq!(validate_dag_labels(DAG), Vec::<String>::new());
    }

    #[test]
    fn validate_accepts_flowchart_lr_and_leading_blank_lines() {
        assert_eq!(
            validate_dag_labels("\n\nflowchart LR\nN1[\"Nodo 1: a b\"]"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn validate_rejects_a_missing_header() {
        let errors = validate_dag_labels("N1[\"Nodo 1: a\"]");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("line 1"), "{errors:?}");
    }

    #[test]
    fn validate_rejects_unnumbered_unquoted_and_mis_numbered_labels_with_line_numbers() {
        let errors =
            validate_dag_labels("graph TD\nN1[\"Primo\"] --> N2[Nodo 2: x]\nN3[\"Nodo 4: y\"]");
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(
            errors[0].contains("line 2") && errors[0].contains("N1"),
            "{errors:?}"
        );
        assert!(
            errors[1].contains("line 2") && errors[1].contains("N2"),
            "{errors:?}"
        );
        assert!(
            errors[2].contains("line 3") && errors[2].contains("N3"),
            "{errors:?}"
        );
    }

    #[test]
    fn validate_requires_at_least_one_plan_node() {
        assert!(validate_dag_labels("graph TD\nA --> B")
            .join(",")
            .contains("no plan node"));
    }

    #[test]
    fn derive_marks_captured_nodes_done_and_the_first_todo_current() {
        let state = derive_plan_state("t", "g", DAG, true, 1, &[]);
        assert_eq!(
            status_list(&state),
            vec![
                ("N1".to_string(), "done".to_string()),
                ("N2".to_string(), "current".to_string()),
                ("N3".to_string(), "todo".to_string()),
            ]
        );
        assert_eq!(state["approved"], json!(true));
    }

    #[test]
    fn derive_nothing_captured_makes_n1_current() {
        assert_eq!(
            status_list(&derive_plan_state("t", "g", DAG, false, 0, &[])),
            vec![
                ("N1".to_string(), "current".to_string()),
                ("N2".to_string(), "todo".to_string()),
                ("N3".to_string(), "todo".to_string()),
            ]
        );
    }

    #[test]
    fn derive_a_failed_latest_check_overrides_done_with_missed() {
        let state = derive_plan_state("t", "g", DAG, true, 2, &[rec(&["N1"], "check", Some(false))]);
        assert_eq!(
            status_list(&state),
            vec![
                ("N1".to_string(), "missed".to_string()),
                ("N2".to_string(), "done".to_string()),
                ("N3".to_string(), "current".to_string()),
            ]
        );
    }

    #[test]
    fn derive_a_later_correct_check_clears_the_miss() {
        let state = derive_plan_state(
            "t",
            "g",
            DAG,
            true,
            1,
            &[rec(&["N1"], "check", Some(false)), rec(&["N1"], "check", Some(true))],
        );
        assert_eq!(state["nodeStatus"]["N1"], json!("done"));
    }

    #[test]
    fn derive_current_follows_the_latest_predict_or_check_when_that_node_is_todo() {
        let state = derive_plan_state("t", "g", DAG, true, 1, &[rec(&["N3"], "predict", Some(true))]);
        assert_eq!(
            status_list(&state),
            vec![
                ("N1".to_string(), "done".to_string()),
                ("N2".to_string(), "todo".to_string()),
                ("N3".to_string(), "current".to_string()),
            ]
        );
    }

    #[test]
    fn derive_probe_records_and_unknown_refs_do_not_move_current() {
        let state = derive_plan_state(
            "t",
            "g",
            DAG,
            false,
            0,
            &[rec(&["N3"], "probe", None), rec(&["Z9"], "check", None)],
        );
        assert_eq!(state["nodeStatus"]["N1"], json!("current"));
    }

    #[test]
    fn derive_keeps_node_plan_state_key_order() {
        let state = derive_plan_state("t", "g", DAG, true, 0, &[]);
        let keys: Vec<&String> = state.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["topic", "goal", "dag", "approved", "nodeStatus"]);
        let ids: Vec<&String> = state["nodeStatus"].as_object().unwrap().keys().collect();
        assert_eq!(ids, vec!["N1", "N2", "N3"]);
    }
}
