//! Reference sheet writer: compile-ref / reference (port of compileReference, extensions/html-lesson.ts:970-1190).

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use crate::html_lesson;
use crate::lesson::{APP_NAME, doc, js, node_path, render};

#[derive(Debug, Clone, Default)]
pub struct CompileReferenceOptions {
    pub lesson_path: Option<String>,
    pub slug: Option<String>,
    pub output_dir: Option<String>,
    pub base_dir: Option<String>,
    pub record_path: Option<String>,
}

/// Writes `<outDir>/<slug>.html` and returns the object the CLI prints, in Node's key order.
pub fn compile_reference(o: &CompileReferenceOptions) -> Result<Value, String> {
    let base = doc::base_dir(&o.base_dir)?;
    let lessons_dir = doc::resolve_string(&Path::new(&base).join("lessons"));
    let out_dir = match doc::truthy(&o.output_dir) {
        Some(dir) => dir.to_string(),
        None => doc::resolve_string(&Path::new(&base).join("reference")),
    };

    let mut file_path = doc::truthy(&o.lesson_path).map(str::to_owned);
    if file_path.is_none()
        && let Some(slug) = doc::truthy(&o.slug)
    {
        file_path = doc::find_lesson_by_slug(&lessons_dir, slug);
    }
    let found = file_path
        .as_deref()
        .is_some_and(|path| Path::new(path).exists());
    if !found {
        // Node prints the resolved lesson path; a slug it could not match shows as undefined.
        return Err(format!(
            "Cannot compile reference: lesson file not found ({})",
            file_path.as_deref().unwrap_or("undefined")
        ));
    }
    let file_path = file_path.expect("checked above");

    let raw = doc::read_text(Path::new(&file_path))?;
    let doc = doc::parse_lesson_document(&raw)?
        .filter(|doc| js::js_truthy(Some(doc)))
        .ok_or_else(|| format!("Corrupted lesson file: missing #lesson-data in {file_path}"))?;
    // Validated before mkdir so a broken island leaves no empty reference/ behind.
    if !doc.get("metadata").is_some_and(Value::is_object)
        || !doc.get("nodes").is_some_and(Value::is_array)
    {
        return Err("Malformed #lesson-data island: expected metadata and nodes".to_string());
    }

    if !Path::new(&out_dir).exists() {
        fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    }
    let out_path = node_path::join(
        &out_dir,
        &format!("{}.html", js::js_concat(doc["metadata"].get("slug"))),
    );

    let sintesi = html_lesson::build_sintesi_document_with_record(
        &doc,
        Path::new(&file_path),
        doc::truthy(&o.record_path),
    )?;
    let html = render_reference_html(&doc, &sintesi);
    fs::write(&out_path, html).map_err(|e| e.to_string())?;

    let nodes = doc["nodes"].as_array().expect("validated nodes");
    let unconditional_truths = nodes
        .iter()
        .filter(|node| matches!(tier(node), Some("unconditional-truth" | "axiom")))
        .count();
    let derivations = nodes
        .iter()
        .filter(|node| tier(node) == Some("derivation"))
        .count();
    let count = |key: &str| {
        sintesi
            .get(key)
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
    };

    let mut out = Map::new();
    out.insert("filePath".into(), Value::String(out_path.clone()));
    out.insert("filepath".into(), Value::String(out_path));
    // JSON.stringify drops an undefined slug.
    if let Some(slug) = doc["metadata"].get("slug") {
        out.insert("slug".into(), slug.clone());
    }
    out.insert(
        "unconditionalTruthCount".into(),
        Value::from(unconditional_truths),
    );
    out.insert("axiomsCount".into(), Value::from(unconditional_truths));
    out.insert("formulaCount".into(), Value::from(count("formulas")));
    out.insert("nodeCount".into(), Value::from(nodes.len()));
    out.insert("derivationCount".into(), Value::from(derivations));
    out.insert(
        "misconceptionsCount".into(),
        Value::from(count("misconceptions")),
    );
    out.insert("document".into(), sintesi);
    Ok(Value::Object(out))
}

/// Byte-for-byte port of compileReference's page literal (:1003-1174).
pub fn render_reference_html(doc: &Value, sintesi: &Value) -> String {
    let empty = Vec::new();
    let nodes = sintesi
        .get("nodes")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let nodes_html = if nodes.is_empty() {
        "<p>Nessun nodo registrato.</p>".to_string()
    } else {
        nodes.iter().map(node_card).collect::<Vec<_>>().join("\n")
    };

    let formulas = sintesi
        .get("formulas")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let formulas_html = if formulas.is_empty() {
        "<p>Nessuna formula registrata.</p>".to_string()
    } else {
        formulas
            .iter()
            .map(formula_card)
            .collect::<Vec<_>>()
            .join("\n")
    };

    let misconceptions = sintesi
        .get("misconceptions")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let misconceptions_html = if misconceptions.is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"card misconceptions-card\">\n  <h2>Errori e dubbi emersi nella sessione</h2>\n  <ul class=\"misconceptions-list\">\n{}  </ul>\n</div>\n",
            misconceptions
                .iter()
                .map(misconception_item)
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let title = sintesi.get("title");
    let dag = doc
        .get("metadata")
        .and_then(|metadata| metadata.get("initialDag"))
        .filter(|dag| js::js_truthy(Some(dag)));

    let mut html = String::new();
    html.push_str(PAGE_OPEN);
    html.push_str(&esc(title));
    html.push_str(" | ");
    html.push_str(APP_NAME);
    html.push_str(PAGE_HEAD);
    html.push_str(&js::stringify_compact(sintesi));
    html.push_str(PAGE_MAIN);
    html.push_str(&esc(title));
    html.push_str(PAGE_NODES);
    html.push_str(&nodes_html);
    html.push_str("\n    </section>\n\n");
    if let Some(dag) = dag {
        html.push_str("    <div class=\"diagram-container\">\n      <h2>Grafo delle dipendenze</h2>\n      <pre class=\"mermaid\">");
        html.push_str(&js::js_concat(Some(dag)));
        html.push_str("</pre>\n    </div>\n");
    }
    html.push('\n');
    html.push_str(&misconceptions_html);
    html.push_str(PAGE_FORMULAS);
    html.push_str(&formulas_html);
    html.push_str(PAGE_END);
    html
}

fn tier(node: &Value) -> Option<&str> {
    node.get("tier").and_then(Value::as_str)
}

fn node_card(node: &Value) -> String {
    let number = js::js_concat(node.get("number"));
    let number = format!(
        "{}{number}",
        "0".repeat(2usize.saturating_sub(number.chars().count()))
    );
    let tier = js::js_concat(node.get("tier"));

    let mut snippet = String::new();
    if js::js_truthy(node.get("keyFormula")) {
        snippet.push_str("<div class=\"formula-card\"><div>");
        snippet.push_str(&display_math(&js::js_concat(node.get("keyFormula"))));
        snippet.push_str("</div></div>");
    }
    if js::js_truthy(node.get("keyCode")) {
        let code = &node["keyCode"];
        snippet.push_str("<div class=\"code-card\"><pre><code>");
        snippet.push_str(&esc(code.get("code")));
        snippet.push_str("</code></pre>");
        if js::js_truthy(code.get("caption")) {
            snippet.push_str("<small class=\"code-caption\">");
            snippet.push_str(&esc(code.get("caption")));
            snippet.push_str("</small>");
        }
        snippet.push_str("</div>");
    }

    format!(
        "<div class=\"node-card tier-{tier}\">\n  <div class=\"node-card-header\">\n    <div class=\"node-card-title-group\">\n      <span class=\"node-card-num\">Nodo {number}</span>\n      <h3 class=\"node-card-title\">{title}</h3>\n    </div>\n    {badge}\n  </div>\n  <div class=\"node-card-claim\">{claim}</div>\n  {snippet}\n</div>",
        title = esc(node.get("title")),
        badge = tier_badge(&tier),
        claim = render::parse_markdown_prose(js::text(node.get("summary"))),
    )
}

fn formula_card(formula: &Value) -> String {
    format!(
        "<div class=\"formula-card\">\n  <div>{}</div>\n  <small style=\"color: #64748b;\">Fonte: {}</small>\n</div>",
        display_math(&js::js_concat(formula.get("math"))),
        esc(formula.get("sourceNode"))
    )
}

fn misconception_item(item: &Value) -> String {
    let source = if js::js_truthy(item.get("sourceNode")) {
        format!("<strong>{}: </strong>", esc(item.get("sourceNode")))
    } else {
        String::new()
    };
    format!("<li>{source}{}</li>", esc(item.get("description")))
}

fn tier_badge(tier: &str) -> &'static str {
    if tier == "derivation" {
        "<span class=\"badge badge-deriv\">Derivazione</span>"
    } else if tier == "axiom" {
        "<span class=\"badge badge-truth\">Assioma</span>"
    } else {
        "<span class=\"badge badge-truth\">Verità incondizionata</span>"
    }
}

fn display_math(math: &str) -> String {
    if math.starts_with("$$") && math.ends_with("$$") {
        math.to_string()
    } else {
        format!("$$ {math} $$")
    }
}

/// Node's `escapeHtml`: falsy values render as nothing.
fn esc(v: Option<&Value>) -> String {
    match v {
        Some(value) if js::js_truthy(Some(value)) => render::escape_html(&js::js_to_string(value)),
        _ => String::new(),
    }
}

const PAGE_OPEN: &str = concat!(
    "<!DOCTYPE html>\n",
    "<html lang=\"it\" data-theme=\"light\">\n",
    "<head>\n",
    "  <meta charset=\"UTF-8\">\n",
    "  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n",
    "  <title>Sintesi: ",
);

const PAGE_HEAD: &str = concat!(
    "</title>\n",
    "  <link rel=\"stylesheet\" href=\"https://cdn.jsdelivr.net/npm/katex@0.16.11/dist/katex.min.css\" crossorigin=\"anonymous\">\n",
    "  <script defer src=\"https://cdn.jsdelivr.net/npm/katex@0.16.11/dist/katex.min.js\" crossorigin=\"anonymous\"></script>\n",
    "  <script defer src=\"https://cdn.jsdelivr.net/npm/katex@0.16.11/dist/contrib/auto-render.min.js\" crossorigin=\"anonymous\"></script>\n",
    "  <script src=\"https://cdn.jsdelivr.net/npm/mermaid@10/dist/mermaid.min.js\"></script>\n",
    "  <style>\n",
    "    :root {\n",
    "      --font-body: Charter, Georgia, serif, -apple-system, sans-serif;\n",
    "      --font-ui: -apple-system, BlinkMacSystemFont, \"Segoe UI\", Roboto, sans-serif;\n",
    "      --font-mono: ui-monospace, \"SF Mono\", monospace;\n",
    "      --bg: #ffffff;\n",
    "      --fg: #0f172a;\n",
    "      --card-bg: #f8fafc;\n",
    "      --border: #cbd5e1;\n",
    "      --accent: #1e40af;\n",
    "      --truth: #047857;\n",
    "      --truth-bg: #ecfdf5;\n",
    "      --derive: #2563eb;\n",
    "      --derive-bg: #eff6ff;\n",
    "      --warn: #b45309;\n",
    "      --warn-bg: #fffbeb;\n",
    "    }\n",
    "    body { background: var(--bg); color: var(--fg); font-family: var(--font-body); margin: 0; padding: 2.5rem 1.5rem; line-height: 1.6; }\n",
    "    .ref-container { max-width: 960px; margin: 0 auto; }\n",
    "    header { margin-bottom: 2rem; border-bottom: 2px solid var(--accent); padding-bottom: 0.75rem; }\n",
    "    h1 { font-family: var(--font-ui); font-size: 2.2rem; margin: 0 0 0.5rem 0; color: var(--fg); }\n",
    "    .subtitle { font-size: 1rem; color: #64748b; margin: 0; }\n",
    "    .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); gap: 1.5rem; margin-bottom: 2rem; }\n",
    "    .card { background: var(--card-bg); border: 1px solid var(--border); border-radius: 8px; padding: 1.5rem; margin-bottom: 1.5rem; }\n",
    "    .card h2 { font-family: var(--font-ui); font-size: 1.25rem; margin-top: 0; color: var(--accent); border-bottom: 1px solid var(--border); padding-bottom: 0.5rem; }\n",
    "    .node-card { background: #ffffff; border: 1px solid var(--border); border-radius: 8px; padding: 1.25rem; margin-bottom: 1rem; }\n",
    "    .node-card.tier-unconditional-truth, .node-card.tier-axiom { border-left: 4px solid var(--truth); }\n",
    "    .node-card.tier-derivation { border-left: 4px solid var(--derive); }\n",
    "    .node-card-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; margin-bottom: 0.75rem; }\n",
    "    .node-card-title-group { display: flex; flex-direction: column; gap: 0.25rem; }\n",
    "    .node-card-num { font-family: var(--font-mono); font-size: 0.75rem; color: #64748b; text-transform: uppercase; letter-spacing: 0.05em; }\n",
    "    .node-card-title { font-family: var(--font-ui); font-size: 1.1rem; margin: 0; color: var(--fg); }\n",
    "    .badge { font-family: var(--font-ui); font-size: 0.7rem; font-weight: 600; padding: 0.2rem 0.5rem; border-radius: 4px; white-space: nowrap; }\n",
    "    .badge-truth { background: var(--truth-bg); color: var(--truth); }\n",
    "    .badge-deriv { background: var(--derive-bg); color: var(--derive); }\n",
    "    .node-card-claim { font-size: 0.95rem; line-height: 1.5; color: #334155; margin-bottom: 0.75rem; }\n",
    "    .formula-card { background: #f8fafc; border: 1px solid var(--border); border-radius: 4px; padding: 0.75rem; margin-bottom: 0.75rem; text-align: center; }\n",
    "    .code-card { background: #0f172a; color: #f8fafc; border-radius: 4px; padding: 0.75rem 1rem; margin-top: 0.5rem; overflow-x: auto; }\n",
    "    .code-card pre { margin: 0; font-family: var(--font-mono); font-size: 0.85rem; }\n",
    "    .code-caption { display: block; color: #94a3b8; font-size: 0.75rem; margin-top: 0.25rem; }\n",
    "    .diagram-container { background: #ffffff; border: 1px solid var(--border); border-radius: 8px; padding: 1.5rem; margin-bottom: 1.5rem; }\n",
    "    .diagram-container h2 { font-family: var(--font-ui); font-size: 1.25rem; margin-top: 0; color: var(--accent); border-bottom: 1px solid var(--border); padding-bottom: 0.5rem; }\n",
    "    .misconceptions-card { border-left: 4px solid var(--warn); background: var(--warn-bg); }\n",
    "    .misconceptions-card h2 { color: var(--warn); }\n",
    "    .misconceptions-list { margin: 0.5rem 0 0 1.25rem; padding: 0; font-size: 0.9rem; }\n",
    "    .misconceptions-list li { margin-bottom: 0.35rem; }\n",
    "    @media print {\n",
    "      body { padding: 0; }\n",
    "      .card, .node-card, .diagram-container { border: 1px solid #000; break-inside: avoid; }\n",
    "    }\n",
    "  </style>\n",
    "</head>\n",
    "<body>\n",
    "  <script type=\"application/json\" id=\"reference-data\">",
);

const PAGE_MAIN: &str = concat!(
    "</script>\n",
    "  <main class=\"ref-container\">\n",
    "    <header>\n",
    "      <h1>Sintesi: ",
);

const PAGE_NODES: &str = concat!(
    "</h1>\n",
    "      <p class=\"subtitle\">Sintesi rapida per il ripasso: concetti chiave, derivazioni, grafo ed inventario formule.</p>\n",
    "    </header>\n",
    "\n",
    "    <section class=\"card sintesi-nodes\">\n",
    "      <h2>Nodi concettuali</h2>\n",
    "      ",
);

const PAGE_FORMULAS: &str = concat!(
    "\n    <div class=\"card\">\n",
    "      <h2>Inventario formule</h2>\n",
    "      ",
);

const PAGE_END: &str = concat!(
    "\n    </div>\n",
    "  </main>\n",
    "\n",
    "  <script>\n",
    "    document.addEventListener(\"DOMContentLoaded\", function() {\n",
    "      if (typeof renderMathInElement === \"function\") {\n",
    "        renderMathInElement(document.body, {\n",
    "          delimiters: [\n",
    "            {left: \"$$\", right: \"$$\", display: true},\n",
    "            {left: \"$\", right: \"$\", display: false},\n",
    "            {left: \"\\(\", right: \"\\)\", display: false},\n",
    "            {left: \"\\[\", right: \"\\]\", display: true}\n",
    "          ],\n",
    "          throwOnError: false\n",
    "        });\n",
    "      }\n",
    "      if (typeof mermaid !== \"undefined\") {\n",
    "        mermaid.initialize({\n",
    "          startOnLoad: true,\n",
    "          fontFamily: '-apple-system, BlinkMacSystemFont, \"Segoe UI\", Roboto, sans-serif'\n",
    "        });\n",
    "      }\n",
    "    });\n",
    "  </script>\n",
    "</body>\n",
    "</html>",
);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::env;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

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
        let path = env::temp_dir().join(format!(
            "proxi-lesson-reference-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn repo_root() -> PathBuf {
        crate::data_root::resolve_path(&Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".."))
    }

    fn as_string(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn init_lesson(dir: &Path, topic: &str, title: &str, dag: Option<&str>) -> doc::InitResult {
        doc::init_lesson(&doc::InitOptions {
            topic: topic.to_string(),
            title: Some(title.to_string()),
            slug: None,
            description: None,
            dag: dag.map(str::to_owned),
            target_goal: None,
            base_dir: Some(as_string(dir)),
            lessons_dir: None,
            repo_root: repo_root(),
        })
        .expect("init lesson")
    }

    fn append(dir: &Path, lesson_path: &str, node: Value) {
        doc::append_node(doc::AppendOptions {
            lesson_path: Some(lesson_path.to_string()),
            slug: None,
            node,
            base_dir: Some(as_string(dir)),
        })
        .expect("append node");
    }

    fn options(dir: &Path, lesson_path: &str) -> CompileReferenceOptions {
        CompileReferenceOptions {
            lesson_path: Some(lesson_path.to_string()),
            base_dir: Some(as_string(dir)),
            ..CompileReferenceOptions::default()
        }
    }

    fn write_lesson(dir: &Path, name: &str, html: &str) -> String {
        let lessons = dir.join("lessons");
        fs::create_dir_all(&lessons).expect("lessons dir");
        let path = lessons.join(name);
        fs::write(&path, html).expect("write lesson");
        as_string(&path)
    }

    #[test]
    fn compile_reference_extracts_truths_formulas_and_dag() {
        let tmp = temp_dir("compile");
        let init = init_lesson(
            &tmp.0,
            "Packet Transmission",
            "Networking Fundamentals",
            Some("flowchart TD\n  A[\"Packet Transmission\"] --> B[\"TCP Flow Control\"]"),
        );
        append(
            &tmp.0,
            &init.file_path,
            json!({
                "nodeId": "packet-axiom",
                "title": "Packet Transmission Axiom",
                "tier": "unconditional-truth",
                "motivation": "Foundational law of computer networks",
                "content": "ALL communication between computers across networks is performed via packet transmission.",
                "math": ["\\text{Throughput} = \\frac{\\text{Window Size}}{\\text{RTT}}"],
            }),
        );
        append(
            &tmp.0,
            &init.file_path,
            json!({
                "nodeId": "tcp-handshake",
                "title": "3-Way Handshake",
                "tier": "derivation",
                "motivation": "Connection establishment in hostile transport layers",
                "content": "SYN, SYN-ACK, ACK sequence synchronizes sequence numbers between endpoints.",
            }),
        );
        append(
            &tmp.0,
            &init.file_path,
            json!({
                "nodeId": "tcp-teardown",
                "title": "Connection Teardown",
                "tier": "derivation",
                "summary": "FIN e ACK chiudono ordinatamente la connessione.",
                "content": "Spiegazione estesa con molti dettagli tecnici secondari...",
            }),
        );

        let result = compile_reference(&options(&tmp.0, &init.file_path)).expect("compile");

        assert_eq!(result["slug"], json!("networking-fundamentals"));
        assert_eq!(result["unconditionalTruthCount"], json!(1));
        assert_eq!(result["nodeCount"], json!(3));
        assert_eq!(result["derivationCount"], json!(2));

        let file_path = result["filePath"].as_str().expect("filePath");
        assert_eq!(
            Path::new(file_path),
            tmp.0
                .join("reference")
                .join("networking-fundamentals.html")
                .as_path()
        );
        let html = fs::read_to_string(file_path).expect("reference file");
        assert!(html.contains("<title>Sintesi: Networking Fundamentals | Proxi</title>"));
        for needle in [
            "Packet Transmission Axiom",
            "ALL communication between computers",
            "\\text{Throughput}",
            "flowchart TD",
            "@media print",
            "3-Way Handshake",
            "Derivazione",
            "Verità incondizionata",
            "id=\"reference-data\"",
            "FIN e ACK chiudono ordinatamente",
        ] {
            assert!(html.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn compile_reference_result_keys_in_node_order() {
        let tmp = temp_dir("keys");
        let init = init_lesson(&tmp.0, "Topic", "Titolo", None);
        let result = compile_reference(&options(&tmp.0, &init.file_path)).expect("compile");

        let keys: Vec<&str> = result
            .as_object()
            .expect("result object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "filePath",
                "filepath",
                "slug",
                "unconditionalTruthCount",
                "axiomsCount",
                "formulaCount",
                "nodeCount",
                "derivationCount",
                "misconceptionsCount",
                "document",
            ]
        );
    }

    #[test]
    fn reference_island_is_compact_and_unescaped() {
        let tmp = temp_dir("island");
        let init = init_lesson(&tmp.0, "Topic", "A <b>", None);
        let result = compile_reference(&options(&tmp.0, &init.file_path)).expect("compile");

        let html = fs::read_to_string(result["filePath"].as_str().expect("filePath"))
            .expect("reference file");
        let open = "  <script type=\"application/json\" id=\"reference-data\">";
        let start = html.find(open).expect("reference island") + open.len();
        assert!(
            html[start..].starts_with("{\"title\":\"A <b>\""),
            "got {}",
            &html[start..start + 40]
        );
        assert!(html.contains("<h1>Sintesi: A &lt;b&gt;</h1>"));
    }

    #[test]
    fn empty_lesson_renders_placeholders() {
        let tmp = temp_dir("empty");
        let init = init_lesson(&tmp.0, "Base", "Base", None);
        let result = compile_reference(&options(&tmp.0, &init.file_path)).expect("compile");

        let html = fs::read_to_string(result["filePath"].as_str().expect("filePath"))
            .expect("reference file");
        assert!(html.contains("Nessun nodo registrato."));
        assert!(html.contains("Nessuna formula registrata."));
        assert!(!html.contains("Grafo delle dipendenze"));
        assert!(!html.contains("class=\"card misconceptions-card\""));
        for key in [
            "unconditionalTruthCount",
            "axiomsCount",
            "formulaCount",
            "nodeCount",
            "derivationCount",
            "misconceptionsCount",
        ] {
            assert_eq!(result[key], json!(0), "{key}");
        }
    }

    #[test]
    fn record_path_feeds_misconceptions() {
        let tmp = temp_dir("record");
        let init = init_lesson(&tmp.0, "Probabilità", "Probabilità", None);
        let record = tmp.0.join("record.md");
        fs::write(&record, "## Evidence\n- Nodo 1: scambia A con B\n").expect("write record");

        let mut opts = options(&tmp.0, &init.file_path);
        opts.record_path = Some(as_string(&record));
        let result = compile_reference(&opts).expect("compile");

        assert_eq!(result["misconceptionsCount"], json!(1));
        let html = fs::read_to_string(result["filePath"].as_str().expect("filePath"))
            .expect("reference file");
        assert!(html.contains("misconceptions-card"));
        assert!(html.contains("Nodo 1: scambia A con B"));
    }

    #[test]
    fn compile_reference_errors_match_node() {
        let no_target = temp_dir("no-target");
        let err = compile_reference(&CompileReferenceOptions {
            base_dir: Some(as_string(&no_target.0)),
            ..CompileReferenceOptions::default()
        })
        .expect_err("no target");
        assert_eq!(
            err,
            "Cannot compile reference: lesson file not found (undefined)"
        );

        let slug_miss = temp_dir("slug-miss");
        fs::create_dir_all(slug_miss.0.join("lessons")).expect("lessons dir");
        let err = compile_reference(&CompileReferenceOptions {
            slug: Some("nope".to_string()),
            base_dir: Some(as_string(&slug_miss.0)),
            ..CompileReferenceOptions::default()
        })
        .expect_err("slug miss");
        assert_eq!(
            err,
            "Cannot compile reference: lesson file not found (undefined)"
        );
        assert!(!slug_miss.0.join("reference").exists());

        let missing = temp_dir("missing");
        let missing_path = as_string(&missing.0.join("lessons").join("9999-x.html"));
        let err = compile_reference(&options(&missing.0, &missing_path)).expect_err("missing");
        assert_eq!(
            err,
            format!("Cannot compile reference: lesson file not found ({missing_path})")
        );

        let corrupted = temp_dir("corrupted");
        let corrupted_path = write_lesson(&corrupted.0, "0001-rotta.html", "<html></html>");
        let err =
            compile_reference(&options(&corrupted.0, &corrupted_path)).expect_err("corrupted");
        assert_eq!(
            err,
            format!("Corrupted lesson file: missing #lesson-data in {corrupted_path}")
        );

        let null_island = temp_dir("null-island");
        let null_path = write_lesson(
            &null_island.0,
            "0001-null.html",
            "<script type=\"application/json\" id=\"lesson-data\">null</script>",
        );
        let err = compile_reference(&options(&null_island.0, &null_path)).expect_err("null island");
        assert_eq!(
            err,
            format!("Corrupted lesson file: missing #lesson-data in {null_path}")
        );

        let malformed = temp_dir("malformed");
        let malformed_path = write_lesson(
            &malformed.0,
            "0001-broken.html",
            "<script type=\"application/json\" id=\"lesson-data\">{</script>",
        );
        let err =
            compile_reference(&options(&malformed.0, &malformed_path)).expect_err("malformed");
        assert!(err.starts_with("Malformed #lesson-data island: "), "{err}");
        assert!(!malformed.0.join("reference").exists());

        let wrong_shape = temp_dir("wrong-shape");
        let wrong_path = write_lesson(
            &wrong_shape.0,
            "0001-shape.html",
            "<script type=\"application/json\" id=\"lesson-data\">{\"metadata\":[],\"nodes\":{}}</script>",
        );
        let err =
            compile_reference(&options(&wrong_shape.0, &wrong_path)).expect_err("wrong shape");
        assert_eq!(
            err,
            "Malformed #lesson-data island: expected metadata and nodes"
        );
        assert!(!wrong_shape.0.join("reference").exists());
    }
}
