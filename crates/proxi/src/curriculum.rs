//! curriculum.json: the course frame labels of the map (readCurriculum, chapter.ts:60-74).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::js;

/// `slug -> title` for every chapter of the plan; a malformed plan is an error, its absence is not.
pub fn course_labels(data_root: &Path) -> Result<HashMap<String, Value>, String> {
    let file = data_root.join("curriculum.json");
    if !file.exists() {
        return Ok(HashMap::new());
    }
    let raw = fs::read(&file).map_err(|e| e.to_string())?;
    let parsed = js::parse(&String::from_utf8_lossy(&raw)).map_err(|e| e.to_string())?;
    let Some(top) = parsed.as_object() else {
        return Ok(HashMap::new());
    };
    let Some(chapters) = top.get("chapters") else {
        return Ok(HashMap::new());
    };
    // Refuse a wrong shape instead of degrading it to []: planChapter writes the result back.
    let Value::Array(chapters) = chapters else {
        return Err("\"chapters\" must be an array".to_string());
    };

    let mut labels = HashMap::new();
    for chapter in chapters {
        if let Some(c) = chapter.as_object() {
            let key = js::display(c.get("slug"));
            let title = c.get("title").cloned().unwrap_or(Value::Null);
            labels.insert(key, title);
        }
    }
    Ok(labels)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "proxi-curriculum-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_missing_plan_has_no_labels() {
        let root = temp_dir("missing");
        assert!(course_labels(&root).unwrap().is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_slug_and_title_of_every_chapter() {
        let root = temp_dir("two");
        fs::write(
            root.join("curriculum.json"),
            r#"{"chapters":[{"title":"Primo","slug":"primo","lessons":[]},{"title":"Secondo","slug":"secondo"}]}"#,
        )
        .unwrap();
        let labels = course_labels(&root).unwrap();
        assert_eq!(labels.get("primo"), Some(&json!("Primo")));
        assert_eq!(labels.get("secondo"), Some(&json!("Secondo")));
        assert_eq!(labels.len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_chapter_without_title_labels_as_null() {
        let root = temp_dir("no-title");
        fs::write(root.join("curriculum.json"), r#"{"chapters":[{"slug":"x"}]}"#).unwrap();
        let labels = course_labels(&root).unwrap();
        assert_eq!(labels.get("x"), Some(&Value::Null));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn null_chapters_and_invalid_json_are_errors() {
        let root = temp_dir("broken");
        fs::write(root.join("curriculum.json"), r#"{"chapters":null}"#).unwrap();
        assert_eq!(
            course_labels(&root),
            Err("\"chapters\" must be an array".to_string())
        );

        fs::write(root.join("curriculum.json"), "{not json").unwrap();
        assert!(course_labels(&root).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
