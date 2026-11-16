//! Session manager: persistence, engine events to chat items, per-session events (port of `app/server/sessions.ts`).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use axum::http::StatusCode;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use crate::agy;
use crate::anki_client::AnkiClient;
use crate::anki_import::ImportRegistry;
use crate::claude::{self, ClaudeBin, ClaudeLaunch, Grant, McpServer};
use crate::context::{self, PromptInput};
use crate::course_data::valid_course;
use crate::courses::{self, CoursesError};
use crate::engine::{self, EngineEvent, EngineHandle, EngineMsg};
use crate::events::{self, EventBus};
use crate::js;
use crate::knowledge;
use crate::lesson;
use crate::limits::{self, LimitsCache};
use crate::opencode;
use crate::planstate;
use crate::protocol::{
    ANKI_OPT_IN_QUESTION, GlobalEvent, JsNumber, MODELS, ServerEvent, SessionState, SessionStatus,
    Usage, session_summary,
};
use crate::settings;
use crate::slug;
use crate::tools::{self, Cancelled, JobStarter};
use crate::turns::{LastTurn, TurnAction, TurnHost, Turns};

/// A request error the routes turn into `{"error": …}` with this status.
#[derive(Debug)]
pub struct HttpError {
    pub status: StatusCode,
    pub message: String,
}

impl HttpError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::from_u16(status).expect("a known HTTP status"),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HttpError {}

pub struct ManagerOptions {
    pub repo_root: PathBuf,
    pub data_root: PathBuf,
    pub code_root: PathBuf,
    pub port: u16,
    pub token: String,
    pub claude: ClaudeBin,
    pub agy: ClaudeBin,
    pub opencode: ClaudeBin,
    pub add_dirs: Vec<PathBuf>,
    pub events: EventBus,
    pub limits: Arc<LimitsCache>,
    pub anki: AnkiClient,
    pub imports: ImportRegistry,
    pub jobs: Arc<dyn JobStarter>,
    /// The pinned npm flag: the exercise tool runs with the same config as the route.
    pub npm_bin: Option<PathBuf>,
    /// The one-install registry shared with the routes, so a tool run and a UI run share `npm install`.
    pub installs: crate::exercises::Installs,
    /// One per server: the ONNX session and the vector cache load once.
    pub semantic: Arc<crate::semantic::SemanticSearch>,
}

/// Interactive tools: their card is the representation, no chip.
const NO_CHIP: [&str; 4] = [
    "mcp__app__quiz",
    "mcp__app__ask",
    "mcp__app__propose_plan",
    "mcp__app__propose_curriculum",
];

/// How a pending card settles: a tool call waiting on this answer, or the Anki opt-in question.
enum Resolver {
    Tool(oneshot::Sender<Result<Value, Cancelled>>),
    AnkiOptIn,
}

struct PendingReq {
    item_id: String,
    kind: String,
    resolver: Resolver,
}

struct Session {
    data: SessionData,
    turns: Turns,
}

struct SessionData {
    state: SessionState,
    /// Engine pinned at creation: a resume token only means something to the engine that issued it.
    provider: String,
    /// Title of a session whose opening turn was never sent (Anki question unanswered, start failed, app restarted).
    pending_start: Option<String>,
    pending: IndexMap<String, PendingReq>,
    subs: Vec<UnboundedSender<String>>,
    closing: bool,
    /// Teacher items by message id: stream order (for text-block ordinals) and count of blocks finalised.
    streamed: HashMap<String, Vec<String>>,
    finalised: HashMap<String, usize>,
    save_pending: bool,
    /// Deleted: a flush that runs late must not recreate the dir.
    deleted: bool,
    /// The practice problem `problem` opened and `conclude` has not closed: in memory only, like
    /// Node's `WeakMap` (a restart forgets it).
    open_problem: Option<(String, Value)>,
    manager: Weak<Shared>,
}

struct Shared {
    opts: ManagerOptions,
    sessions: Mutex<IndexMap<String, Arc<Mutex<Session>>>>,
    sessions_touch_pending: AtomicBool,
}

#[derive(Clone)]
pub struct SessionManager(Arc<Shared>);

/// Runs `task` after `delay` on the runtime, or on a plain thread when there is none (the manager
/// loads inside `spawn_blocking`, where a runtime may not be in scope).
fn spawn_delayed(delay: Duration, task: impl FnOnce() + Send + 'static) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                tokio::time::sleep(delay).await;
                task();
            });
        }
        Err(_) => {
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                task();
            });
        }
    }
}

/// JS `s.length` of the zod limits: UTF-16 units, not chars.
pub(crate) fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Node's `trunc`: slice to `n` UTF-16 units, then an ellipsis.
pub(crate) fn trunc(text: &str, n: usize) -> String {
    if utf16_len(text) <= n {
        return text.to_string();
    }
    let mut out = String::new();
    let mut units = 0;
    for c in text.chars() {
        let width = c.len_utf16();
        if units + width > n {
            break;
        }
        units += width;
        out.push(c);
    }
    out.push('…');
    out
}

/// One zod issue as Node joins it: `<path>: <message>`.
pub(crate) fn issue(path: &str, message: &str) -> String {
    if path.is_empty() {
        message.to_string()
    } else {
        format!("{path}: {message}")
    }
}

/// zod's type error, `undefined` included: `None` is a missing key.
pub(crate) fn invalid_type(expected: &str, value: Option<&Value>) -> String {
    let received = value.map_or("undefined", js::js_type);
    format!("Invalid input: expected {expected}, received {received}")
}

/// Node's `sessionConfigSchema`: known keys only, in schema order.
pub fn validate_config(body: &Value) -> Result<Map<String, Value>, String> {
    let Some(object) = body.as_object() else {
        return Err("Invalid input: expected object".to_string());
    };
    let mut issues: Vec<String> = Vec::new();
    let mut config = Map::new();

    match object.get("kind") {
        Some(Value::String(kind)) if matches!(kind.as_str(), "teach" | "plan" | "practice") => {
            config.insert("kind".to_string(), Value::String(kind.clone()));
        }
        Some(_) => issues.push(issue(
            "kind",
            "Invalid option: expected one of \"teach\"|\"plan\"|\"practice\"",
        )),
        None => issues.push(issue(
            "kind",
            "Invalid input: expected string, received undefined",
        )),
    }

    match object.get("course") {
        Some(Value::String(course)) if valid_course(course) => {
            config.insert("course".to_string(), Value::String(course.clone()));
        }
        Some(Value::String(_)) => issues.push(issue(
            "course",
            "course must be a kebab-case directory name",
        )),
        Some(_) => issues.push(issue(
            "course",
            "Invalid input: expected string, received non-string",
        )),
        None => issues.push(issue(
            "course",
            "Invalid input: expected string, received undefined",
        )),
    }

    if let Some(value) = object.get("model") {
        match value {
            Value::String(model) if MODELS.contains(&model.as_str()) => {
                config.insert("model".to_string(), value.clone());
            }
            _ => issues.push(issue(
                "model",
                "Invalid option: expected one of \"opus\"|\"sonnet\"|\"haiku\"",
            )),
        }
    }

    if let Some(value) = object.get("practicePropositions") {
        validate_string_array("practicePropositions", value, &mut config, &mut issues);
    }

    if let Some(value) = object.get("target") {
        match value.as_object() {
            None => issues.push(issue("target", "Invalid input: expected object")),
            Some(target) => match target.get("mode").and_then(Value::as_str) {
                Some("planned") => {
                    let mut out = Map::new();
                    out.insert("mode".to_string(), Value::String("planned".to_string()));
                    match target.get("title") {
                        Some(Value::String(title)) if !title.is_empty() => {
                            out.insert("title".to_string(), Value::String(title.clone()));
                        }
                        _ => issues.push(issue(
                            "target.title",
                            "Invalid input: expected a non-empty string",
                        )),
                    }
                    config.insert("target".to_string(), Value::Object(out));
                }
                Some("existing") => {
                    let mut out = Map::new();
                    out.insert("mode".to_string(), Value::String("existing".to_string()));
                    match target.get("lessonPath") {
                        Some(Value::String(path)) if !path.is_empty() => {
                            out.insert("lessonPath".to_string(), Value::String(path.clone()));
                        }
                        _ => issues.push(issue(
                            "target.lessonPath",
                            "Invalid input: expected a non-empty string",
                        )),
                    }
                    config.insert("target".to_string(), Value::Object(out));
                }
                Some("free") => {
                    let mut out = Map::new();
                    out.insert("mode".to_string(), Value::String("free".to_string()));
                    config.insert("target".to_string(), Value::Object(out));
                }
                _ => issues.push(issue(
                    "target.mode",
                    "Invalid option: expected one of \"planned\"|\"existing\"|\"free\"",
                )),
            },
        }
    }

    if let Some(value) = object.get("ankiOptIn") {
        match value {
            Value::Bool(_) => {
                config.insert("ankiOptIn".to_string(), value.clone());
            }
            _ => issues.push(issue("ankiOptIn", "Invalid input: expected boolean")),
        }
    }

    if let Some(value) = object.get("chapterTitle") {
        match value {
            Value::String(title) if !title.is_empty() => {
                config.insert("chapterTitle".to_string(), value.clone());
            }
            _ => issues.push(issue(
                "chapterTitle",
                "Invalid input: expected a non-empty string",
            )),
        }
    }

    if let Some(value) = object.get("practiceNodes") {
        validate_string_array("practiceNodes", value, &mut config, &mut issues);
    }

    if let Some(value) = object.get("note") {
        match value {
            Value::String(note) if utf16_len(note) <= 5000 => {
                config.insert("note".to_string(), value.clone());
            }
            _ => issues.push(issue(
                "note",
                "Too big: expected string to have <=5000 characters",
            )),
        }
    }

    if issues.is_empty() {
        Ok(config)
    } else {
        Err(issues.join("; "))
    }
}

fn validate_string_array(
    key: &str,
    value: &Value,
    config: &mut Map<String, Value>,
    issues: &mut Vec<String>,
) {
    match value.as_array() {
        None => issues.push(issue(key, "Invalid input: expected array")),
        Some(items) if items.iter().all(Value::is_string) => {
            config.insert(key.to_string(), value.clone());
        }
        Some(_) => issues.push(issue(
            key,
            "Invalid input: expected string inside the array",
        )),
    }
}

/// The value's name as zod prints it; `None` is a missing key.
fn js_type(value: Option<&Value>) -> &'static str {
    match value {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

/// `main.ts`'s `textBody`, before the id lookup: zod's issues in schema order, joined as Node does.
fn validate_message(body: &Value) -> Result<(String, Option<Value>), HttpError> {
    let Some(object) = body.as_object() else {
        return Err(HttpError::new(400, "Invalid input: expected object"));
    };
    let mut issues: Vec<String> = Vec::new();
    let text = match object.get("text") {
        Some(Value::String(text)) if (1..=20000).contains(&utf16_len(text)) => Some(text.clone()),
        Some(Value::String(_)) => {
            issues.push(
                "text: Too small or too big: expected string to have >=1 and <=20000 characters"
                    .to_string(),
            );
            None
        }
        _ => {
            issues.push("text: Invalid input: expected string".to_string());
            None
        }
    };
    let attachments = match object.get("attachments") {
        None => None,
        Some(Value::Array(items)) => {
            let mut metas = Vec::new();
            for (index, item) in items.iter().enumerate() {
                let Some(element) = item.as_object() else {
                    issues.push(format!(
                        "attachments.{index}: Invalid input: expected object, received {}",
                        js_type(Some(item))
                    ));
                    continue;
                };
                // A valid one is rebuilt in schema order with its unknown keys dropped.
                let mut meta = Map::new();
                let mut valid = true;
                for key in ["id", "name", "path", "mimeType"] {
                    match element.get(key) {
                        Some(Value::String(value)) => {
                            meta.insert(key.to_string(), Value::String(value.clone()));
                        }
                        other => {
                            valid = false;
                            issues.push(format!(
                                "attachments.{index}.{key}: Invalid input: expected string, received {}",
                                js_type(other)
                            ));
                        }
                    }
                }
                match element.get("size") {
                    Some(Value::Number(value)) => {
                        meta.insert("size".to_string(), Value::Number(value.clone()));
                    }
                    other => {
                        valid = false;
                        issues.push(format!(
                            "attachments.{index}.size: Invalid input: expected number, received {}",
                            js_type(other)
                        ));
                    }
                }
                if valid {
                    metas.push(Value::Object(meta));
                }
            }
            Some(Value::Array(metas))
        }
        Some(other) => {
            issues.push(format!(
                "attachments: Invalid input: expected array, received {}",
                js_type(Some(other))
            ));
            None
        }
    };
    if !issues.is_empty() {
        return Err(HttpError::new(400, issues.join("; ")));
    }
    Ok((text.expect("a valid text"), attachments))
}

/// One uploaded file as the route read it; `mime_type` is the part's content type, when it had one.
pub struct AttachmentUpload {
    pub name: String,
    pub mime_type: Option<String>,
    pub bytes: Vec<u8>,
}

/// Node's allowed upload types (`sessions.ts` `saveAttachments`).
const ALLOWED_ATTACHMENT_EXTS: [&str; 9] = [
    ".png", ".jpg", ".jpeg", ".webp", ".gif", ".svg", ".pdf", ".txt", ".md",
];

/// Node's win32 `path.basename`: what follows the last separator, trailing separators ignored, and
/// no leading drive designator (`C:foo.png` → `foo.png`).
fn attachment_base(raw: &str) -> &str {
    let trimmed = raw.trim_end_matches(|c| c == '/' || c == '\\');
    match trimmed.rfind(|c| c == '/' || c == '\\') {
        Some(index) => &trimmed[index + 1..],
        None => {
            // With no separator left, win32 still drops a `C:` prefix, but only at position 0.
            let bytes = trimmed.as_bytes();
            if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
                &trimmed[2..]
            } else {
                trimmed
            }
        }
    }
}

/// Node's `rawName.replace(/[^A-Za-z0-9._-]/g, "_") || "attachment"`: one `_` per UTF-16 unit.
fn safe_attachment_name(base: &str) -> String {
    let mut out = String::new();
    for c in base.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else {
            for _ in 0..c.len_utf16() {
                out.push('_');
            }
        }
    }
    if out.is_empty() {
        "attachment".to_string()
    } else {
        out
    }
}

/// Node's `path.extname(...).toLowerCase()`: from the last dot, but never the leading one or `..`.
fn attachment_ext(safe: &str) -> String {
    match safe.rfind('.') {
        Some(index) if index > 0 && safe != ".." => safe[index..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Node's `answerBody` (`main.ts:104-114`): the discriminated union, rebuilt in schema key order
/// with unknown keys dropped — that is the object Node hands `manager.answer`.
pub fn validate_answer(body: &Value) -> Result<Value, HttpError> {
    let Some(object) = body.as_object() else {
        return Err(HttpError::new(
            400,
            issue("", &invalid_type("object", Some(body))),
        ));
    };
    let kind = match object.get("kind").and_then(Value::as_str) {
        Some(kind @ ("quiz" | "ask" | "plan" | "curriculum")) => kind,
        _ => {
            return Err(HttpError::new(
                400,
                // zod checks the discriminator before any field.
                "kind: Invalid discriminator value. Expected 'quiz' | 'ask' | 'plan' | 'curriculum'",
            ));
        }
    };

    let mut issues: Vec<String> = Vec::new();
    let mut request_id = None;
    match object.get("requestId") {
        Some(Value::String(text)) => request_id = Some(text.clone()),
        Some(other) => issues.push(string_issue("requestId", Some(other))),
        None => issues.push(string_issue("requestId", None)),
    }
    let answer = match object.get("answer") {
        Some(Value::Object(answer)) => Some(answer_payload(kind, answer, &mut issues)),
        Some(other) => {
            issues.push(issue("answer", &invalid_type("object", Some(other))));
            None
        }
        None => {
            issues.push(issue("answer", &invalid_type("object", None)));
            None
        }
    };
    if !issues.is_empty() {
        return Err(HttpError::new(400, issues.join("; ")));
    }

    let mut out = Map::new();
    out.insert(
        "requestId".to_string(),
        json!(request_id.unwrap_or_default()),
    );
    out.insert("kind".to_string(), json!(kind));
    out.insert("answer".to_string(), answer.unwrap_or(Value::Null));
    Ok(Value::Object(out))
}

/// zod's `received` wording for a string: what the value is, as Node prints it.
fn string_issue(path: &str, value: Option<&Value>) -> String {
    issue(path, &invalid_type("string", value))
}

/// One union branch's `answer`, in that branch's schema key order.
fn answer_payload(kind: &str, answer: &Map<String, Value>, issues: &mut Vec<String>) -> Value {
    let mut out = Map::new();
    if kind == "plan" || kind == "curriculum" {
        match answer.get("approved") {
            Some(Value::Bool(approved)) => {
                out.insert("approved".to_string(), json!(approved));
            }
            Some(other) => issues.push(issue(
                "answer.approved",
                &invalid_type("boolean", Some(other)),
            )),
            None => issues.push(issue("answer.approved", &invalid_type("boolean", None))),
        }
        if let Some(feedback) = optional_string(answer, "feedback", "answer.feedback", issues) {
            out.insert("feedback".to_string(), json!(feedback));
        }
        return Value::Object(out);
    }

    match answer.get("selected") {
        Some(Value::Array(selected)) => {
            let mut labels = Vec::new();
            for (index, label) in selected.iter().enumerate() {
                match label {
                    Value::String(text) => labels.push(text.clone()),
                    other => issues.push(string_issue(
                        &format!("answer.selected.{index}"),
                        Some(other),
                    )),
                }
            }
            out.insert("selected".to_string(), json!(labels));
        }
        Some(other) => issues.push(issue(
            "answer.selected",
            &invalid_type("array", Some(other)),
        )),
        None => issues.push(issue("answer.selected", &invalid_type("array", None))),
    }

    if kind == "quiz" {
        match answer.get("dontKnow") {
            Some(Value::Bool(dont_know)) => {
                out.insert("dontKnow".to_string(), json!(dont_know));
            }
            Some(other) => issues.push(issue(
                "answer.dontKnow",
                &invalid_type("boolean", Some(other)),
            )),
            None => issues.push(issue("answer.dontKnow", &invalid_type("boolean", None))),
        }
        match answer.get("confidence") {
            Some(Value::Number(number)) => {
                let value = number.as_f64().unwrap_or(f64::NAN);
                if value < 0.0 {
                    issues.push(issue(
                        "answer.confidence",
                        "Too small: expected number to be >=0",
                    ));
                } else if value > 100.0 {
                    issues.push(issue(
                        "answer.confidence",
                        "Too big: expected number to be <=100",
                    ));
                } else {
                    out.insert("confidence".to_string(), json!(number));
                }
            }
            Some(other) => issues.push(issue(
                "answer.confidence",
                &invalid_type("number", Some(other)),
            )),
            None => {}
        }
        if let Some(note) = optional_string(answer, "note", "answer.note", issues) {
            out.insert("note".to_string(), json!(note));
        }
        return Value::Object(out);
    }

    if let Some(other) = optional_string(answer, "other", "answer.other", issues) {
        out.insert("other".to_string(), json!(other));
    }
    Value::Object(out)
}

/// `z.string().max(5000).optional()`: absent stays away, a too-long string is an issue.
fn optional_string(
    answer: &Map<String, Value>,
    key: &str,
    path: &str,
    issues: &mut Vec<String>,
) -> Option<String> {
    match answer.get(key) {
        Some(Value::String(text)) => {
            if utf16_len(text) > 5000 {
                issues.push(issue(
                    path,
                    "Too big: expected string to have <=5000 characters",
                ));
                None
            } else {
                Some(text.clone())
            }
        }
        Some(other) => {
            issues.push(string_issue(path, Some(other)));
            None
        }
        None => None,
    }
}

/// The persisted wrapper, in Node's key order; absent optionals are omitted like `undefined`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StateFile<'a> {
    state: &'a SessionState,
    provider: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    resume: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_start: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_turn: Option<&'a LastTurn>,
    can_retry: bool,
}

impl SessionData {
    fn item_index(&self, id: &str) -> Option<usize> {
        self.state
            .items
            .iter()
            .position(|item| item.get("id").and_then(Value::as_str) == Some(id))
    }

    /// Node's `upsert`: an item already in the list is replaced (so its new key order wins), an
    /// unknown one is appended.
    fn upsert(&mut self, item: Value) {
        let id = item.get("id").cloned().unwrap_or(Value::Null);
        let index = self
            .state
            .items
            .iter()
            .position(|existing| existing.get("id") == Some(&id));
        match index {
            Some(index) => self.state.items[index] = item.clone(),
            None => self.state.items.push(item.clone()),
        }
        self.notify(ServerEvent::Item { item });
    }

    fn notify(&mut self, event: ServerEvent) {
        self.state.updated_at = events::iso_now();
        self.schedule_save();
        let Ok(payload) = serde_json::to_string(&event) else {
            return;
        };
        self.subs
            .retain(|sender| sender.send(payload.clone()).is_ok());
    }

    fn schedule_save(&mut self) {
        if self.save_pending {
            return;
        }
        self.save_pending = true;
        let Some(shared) = self.manager.upgrade() else {
            return;
        };
        let id = self.state.id.clone();
        spawn_delayed(Duration::from_millis(300), move || {
            shared.flush_if_pending(&id);
        });
    }

    fn touch_sessions(&mut self) {
        if let Some(shared) = self.manager.upgrade() {
            shared.touch_sessions();
        }
    }

    fn set_pending(&mut self) {
        let last = self.pending.keys().last().cloned();
        self.state.pending_request_id = last.clone();
        self.notify(ServerEvent::Pending { request_id: last });
    }

    fn apply_status(&mut self, status: SessionStatus, error: Option<String>, retryable: bool) {
        self.state.status = status;
        self.state.last_error = if status == SessionStatus::Error {
            error.clone()
        } else {
            None
        };
        self.state.retryable = retryable;
        self.touch_sessions();
        self.notify(ServerEvent::Status {
            status,
            error,
            retryable,
        });
    }

    /// Cancels every pending card, or only the given request ids (a dropped HTTP call).
    fn cancel_pending_only(&mut self, only: Option<&[String]>) {
        let keys: Vec<String> = self
            .pending
            .keys()
            .filter(|key| only.is_none_or(|only| only.contains(key)))
            .cloned()
            .collect();
        let mut cancelled_opt_in = false;
        for request_id in keys {
            let Some(pending) = self.pending.shift_remove(&request_id) else {
                continue;
            };
            if let Some(index) = self.item_index(&pending.item_id) {
                let item = &mut self.state.items[index];
                if item.get("requestId").is_some() {
                    item["cancelled"] = Value::Bool(true);
                    let item = item.clone();
                    self.notify(ServerEvent::Item { item });
                }
            }
            match pending.resolver {
                Resolver::Tool(tx) => {
                    // A dropped receiver means the HTTP call already went away.
                    let _ = tx.send(Err(Cancelled));
                }
                // The rejected `askAnkiOptIn` promise lands here: nothing ran, so the session is idle again.
                Resolver::AnkiOptIn if !self.closing => cancelled_opt_in = true,
                Resolver::AnkiOptIn => {}
            }
        }
        if self.state.pending_request_id.is_some() {
            self.set_pending();
        }
        if self.state.status == SessionStatus::Waiting && self.pending.is_empty() {
            let retryable = self.state.retryable;
            self.apply_status(SessionStatus::Running, None, retryable);
        }
        if cancelled_opt_in {
            let retryable = self.state.retryable;
            self.apply_status(SessionStatus::Idle, None, retryable);
        }
    }

    fn notice(&mut self, level: &str, text: &str) {
        self.upsert(json!({
            "id": format!("n:{}", engine::uuid_v4()),
            "at": events::iso_now(),
            "kind": "notice",
            "level": level,
            "text": text,
        }));
    }

    fn learner(&mut self, text: &str, attachments: Option<&Value>) {
        let mut item = Map::new();
        item.insert("id".to_string(), json!(format!("l:{}", engine::uuid_v4())));
        item.insert("at".to_string(), json!(events::iso_now()));
        item.insert("kind".to_string(), json!("learner"));
        item.insert("text".to_string(), json!(text));
        if let Some(attachments) = attachments
            .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
        {
            item.insert("attachments".to_string(), attachments.clone());
        }
        self.upsert(Value::Object(item));
    }

    /// Node's `onEvent`: engine events to chat items and usage.
    fn event(&mut self, event: &EngineEvent, late: bool) {
        match event {
            EngineEvent::Init { mcp_servers, .. } => {
                self.schedule_save(); // a new resume token
                let app = mcp_servers
                    .iter()
                    .find(|server| server.name == "app" && server.status != "connected");
                if let Some(app) = app {
                    let text = format!(
                        "Il server degli strumenti non è connesso (stato: {}).",
                        app.status
                    );
                    self.notice("error", &text);
                }
            }
            EngineEvent::TextStart { message_id, index } => {
                self.teacher_item(message_id, *index);
            }
            EngineEvent::TextDelta {
                message_id,
                index,
                text,
            } => {
                let id = self.teacher_item(message_id, *index);
                if let Some(index) = self.item_index(&id) {
                    let item = &mut self.state.items[index];
                    let current = item.get("text").and_then(Value::as_str).unwrap_or("");
                    item["text"] = Value::String(format!("{current}{text}"));
                }
                self.notify(ServerEvent::Delta {
                    item_id: id,
                    text: text.clone(),
                });
            }
            EngineEvent::Assistant {
                message_id,
                texts,
                tool_uses,
            } => {
                for (_, text) in texts {
                    self.finalise(message_id, text);
                }
                for tool in tool_uses {
                    if NO_CHIP.contains(&tool.name.as_str()) {
                        continue;
                    }
                    let mut item = Map::new();
                    item.insert("id".to_string(), json!(format!("tool:{}", tool.id)));
                    item.insert("at".to_string(), json!(events::iso_now()));
                    item.insert("kind".to_string(), json!("tool"));
                    item.insert("tool".to_string(), json!(tool.name));
                    if let Some(input) = &tool.input {
                        item.insert("input".to_string(), input.clone());
                    }
                    item.insert("status".to_string(), json!("running"));
                    self.upsert(Value::Object(item));
                }
            }
            EngineEvent::ToolResult {
                tool_use_id,
                is_error,
                text,
            } => {
                let id = format!("tool:{tool_use_id}");
                let tool_item = self.item_index(&id).filter(|index| {
                    self.state.items[*index].get("kind").and_then(Value::as_str) == Some("tool")
                });
                if let Some(index) = tool_item {
                    let item = &mut self.state.items[index];
                    let status = if *is_error { "error" } else { "done" };
                    item["status"] = Value::String(status.to_string());
                    item["resultText"] = Value::String(trunc(text, 2000));
                    let item = item.clone();
                    self.upsert(item);
                }
            }
            EngineEvent::Result {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                ..
            } => {
                {
                    let usage = &mut self.state.usage;
                    usage.input_tokens = JsNumber(usage.input_tokens.0 + input_tokens);
                    usage.output_tokens = JsNumber(usage.output_tokens.0 + output_tokens);
                    usage.cache_read_tokens =
                        JsNumber(usage.cache_read_tokens.0 + cache_read_tokens);
                    usage.cache_creation_tokens =
                        JsNumber(usage.cache_creation_tokens.0 + cache_creation_tokens);
                    usage.turns = JsNumber(usage.turns.0 + 1.0);
                }
                let usage = self.state.usage.clone();
                self.notify(ServerEvent::Usage { usage });
                // Late result of a given-up turn while a newer one runs: usage counted, state untouched.
                if late {
                    return;
                }
                self.streamed.clear();
                self.finalised.clear();
                let ids: Vec<String> = self
                    .state
                    .items
                    .iter()
                    .filter(|item| {
                        item.get("kind").and_then(Value::as_str) == Some("teacher")
                            && item.get("streaming").and_then(Value::as_bool) == Some(true)
                    })
                    .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
                    .collect();
                for id in ids {
                    if let Some(index) = self.item_index(&id) {
                        self.state.items[index]["streaming"] = Value::Bool(false);
                        let item = self.state.items[index].clone();
                        self.upsert(item);
                    }
                }
            }
            _ => {}
        }
    }

    fn teacher_item(&mut self, message_id: &str, index: f64) -> String {
        let id = format!("t:{message_id}:{}", js::js_number_string(index));
        if self.item_index(&id).is_none() {
            self.streamed
                .entry(message_id.to_string())
                .or_default()
                .push(id.clone());
            self.upsert(json!({
                "id": id,
                "at": events::iso_now(),
                "kind": "teacher",
                "text": "",
                "streaming": true,
            }));
        }
        id
    }

    fn finalise(&mut self, message_id: &str, text: &str) {
        let k = self.finalised.get(message_id).copied().unwrap_or(0);
        self.finalised.insert(message_id.to_string(), k + 1);
        let id = self
            .streamed
            .get(message_id)
            .and_then(|ids| ids.get(k))
            .cloned()
            .unwrap_or_else(|| format!("t:{message_id}:f{k}"));
        let item = match self.item_index(&id) {
            Some(index) => {
                let item = &mut self.state.items[index];
                item["text"] = Value::String(text.to_string());
                item["streaming"] = Value::Bool(false);
                item.clone()
            }
            None => json!({
                "id": id,
                "at": events::iso_now(),
                "kind": "teacher",
                "text": text,
                "streaming": false,
            }),
        };
        self.upsert(item);
    }
}

impl TurnHost for SessionData {
    fn status(&self) -> SessionStatus {
        self.state.status
    }

    fn set_status(&mut self, status: SessionStatus, error: Option<String>, retryable: bool) {
        self.apply_status(status, error, retryable);
    }

    fn notice(&mut self, level: &str, text: &str) {
        SessionData::notice(self, level, text);
    }

    fn learner(&mut self, text: &str, attachments: Option<&Value>) {
        SessionData::learner(self, text, attachments);
    }

    fn event(&mut self, event: &EngineEvent, late: bool) {
        SessionData::event(self, event, late);
    }

    fn pending(&self) -> usize {
        self.pending.len()
    }

    fn cancel_pending(&mut self) {
        self.cancel_pending_only(None);
    }

    fn owed_jobs(&self) -> Vec<Value> {
        let Some(shared) = self.manager.upgrade() else {
            return Vec::new();
        };
        shared
            .opts
            .jobs
            .list_for(&self.state.id)
            .into_iter()
            .filter(crate::turns::is_owed)
            .collect()
    }

    fn mark_delivered(&mut self, id: &str) {
        if let Some(shared) = self.manager.upgrade() {
            shared.opts.jobs.mark_delivered(id);
        }
    }
}

impl Shared {
    fn sessions_root(&self) -> PathBuf {
        self.opts.repo_root.join(".learner").join("sessions")
    }

    fn session_dir(&self, id: &str) -> PathBuf {
        self.sessions_root().join(id)
    }

    /// Debounced (300 ms) app-wide hint: the sessions list moved.
    fn touch_sessions(self: &Arc<Self>) {
        if self.sessions_touch_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(self);
        spawn_delayed(Duration::from_millis(300), move || {
            let Some(shared) = weak.upgrade() else {
                return;
            };
            shared.sessions_touch_pending.store(false, Ordering::SeqCst);
            shared.opts.events.emit(&GlobalEvent::Sessions);
        });
    }

    fn flush_if_pending(&self, id: &str) {
        let Some(session) = self
            .sessions
            .lock()
            .expect("sessions lock")
            .get(id)
            .cloned()
        else {
            return;
        };
        let mut guard = session.lock().expect("session lock");
        if !guard.data.save_pending {
            return; // an explicit flush already wrote this round
        }
        let Session { data, turns } = &mut *guard;
        flush(self, data, turns);
    }

    fn session(&self, id: &str) -> Option<Arc<Mutex<Session>>> {
        self.sessions
            .lock()
            .expect("sessions lock")
            .get(id)
            .cloned()
    }

    /// The interrupt watchdog of `id`: the turn never reported its result, so give it up.
    fn watchdog_fired(&self, id: &str, ticket: u64) {
        let Some(session) = self.session(id) else {
            return;
        };
        let mut guard = session.lock().expect("session lock");
        let Session { data, turns } = &mut *guard;
        turns.watchdog_fired(data, ticket);
    }
}

/// The request ids one `tools/call` created: the route cancels exactly these when it is dropped.
pub type CallRequests = Arc<Mutex<Vec<String>>>;

/// One session as the tools see it: port of Node's `makeContext` (`sessions.ts:319-379`). Every
/// method takes the session lock briefly and never holds it across an await.
pub struct SessionToolHost {
    shared: Arc<Shared>,
    session: Arc<Mutex<Session>>,
    call: CallRequests,
    id: String,
}

/// Node's `{...item, …fields}`: the item's keys keep their place, the fields replace or append.
fn spread(item: Value, fields: Vec<(&str, Value)>) -> Value {
    let mut full = match item {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    for (key, value) in fields {
        full.insert(key.to_string(), value);
    }
    Value::Object(full)
}

/// Node's `emit`, with the session lock already held.
fn emit_locked(data: &mut SessionData, item: Value) -> String {
    let id = match item.get("id").and_then(Value::as_str) {
        Some(id) => id.to_string(),
        None => engine::uuid_v4(),
    };
    let item = spread(
        item,
        vec![("id", json!(id)), ("at", json!(events::iso_now()))],
    );
    data.upsert(item);
    id
}

impl tools::ToolHost for SessionToolHost {
    fn data_root(&self) -> &Path {
        &self.shared.opts.data_root
    }

    fn repo_root(&self) -> &Path {
        &self.shared.opts.repo_root
    }

    fn config(&self) -> Map<String, Value> {
        self.session
            .lock()
            .expect("session lock")
            .data
            .state
            .config
            .clone()
    }

    fn lesson_path(&self) -> Option<String> {
        self.session
            .lock()
            .expect("session lock")
            .data
            .state
            .lesson_path
            .clone()
    }

    fn plan(&self) -> Value {
        self.session
            .lock()
            .expect("session lock")
            .data
            .state
            .plan
            .clone()
    }

    fn set_plan(&self, plan: Value) {
        let mut guard = self.session.lock().expect("session lock");
        guard.data.state.plan = plan.clone();
        guard.data.notify(ServerEvent::Plan { plan });
    }

    fn set_lesson(&self, lesson_path: String, node_id: Option<String>) {
        let mut guard = self.session.lock().expect("session lock");
        guard.data.state.lesson_path = Some(lesson_path.clone());
        guard.data.notify(ServerEvent::Lesson {
            lesson_path,
            node_id,
        });
    }

    fn request(
        &self,
        item: Value,
    ) -> Result<oneshot::Receiver<Result<Value, Cancelled>>, Cancelled> {
        let mut guard = self.session.lock().expect("session lock");
        if guard.data.closing || !guard.turns.live() {
            return Err(Cancelled);
        }
        // At most one pending interactive request at a time: a newer card supersedes the older ones.
        if !guard.data.pending.is_empty() {
            guard.data.cancel_pending_only(None);
        }
        let request_id = engine::uuid_v4();
        self.call
            .lock()
            .expect("call lock")
            .push(request_id.clone());
        let item_id = format!("req:{request_id}");
        let item = spread(
            item,
            vec![
                ("id", json!(item_id)),
                ("at", json!(events::iso_now())),
                ("requestId", json!(request_id.clone())),
            ],
        );
        let kind = item
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let (tx, rx) = oneshot::channel();
        let Session { data, turns } = &mut *guard;
        data.pending.insert(
            request_id,
            PendingReq {
                item_id,
                kind,
                resolver: Resolver::Tool(tx),
            },
        );
        data.upsert(item);
        data.set_pending();
        let retryable = turns.can_retry();
        data.apply_status(SessionStatus::Waiting, None, retryable);
        Ok(rx)
    }

    fn emit(&self, item: Value) -> String {
        let mut guard = self.session.lock().expect("session lock");
        emit_locked(&mut guard.data, item)
    }

    fn exercises(&self) -> Vec<Value> {
        self.session
            .lock()
            .expect("session lock")
            .data
            .state
            .exercises
            .clone()
    }

    fn upsert_exercise(&self, info: Value) {
        let mut guard = self.session.lock().expect("session lock");
        let name = js::js_string(info.get("name"));
        let mut list: Vec<Value> = guard
            .data
            .state
            .exercises
            .clone()
            .into_iter()
            .filter(|entry| js::js_string(entry.get("name")) != name)
            .collect();
        list.push(info.clone());
        list.sort_by(|left, right| {
            js::locale_compare(
                &js::js_string(left.get("name")),
                &js::js_string(right.get("name")),
            )
        });
        guard.data.state.exercises = list.clone();
        guard
            .data
            .notify(ServerEvent::Exercises { exercises: list });
        emit_locked(
            &mut guard.data,
            json!({
                "id": format!("exercise:{name}"),
                "kind": "exercise",
                "exercise": info,
            }),
        );
    }

    fn open_problem(&self, item: Value) -> Option<String> {
        let mut guard = self.session.lock().expect("session lock");
        let data = &mut guard.data;
        if data.open_problem.is_some() {
            return None;
        }
        let input = item.get("input").cloned().unwrap_or(Value::Null);
        let id = emit_locked(data, item);
        data.open_problem = Some((id.clone(), input));
        Some(id)
    }

    fn take_problem(&self) -> Option<(String, Value)> {
        self.session
            .lock()
            .expect("session lock")
            .data
            .open_problem
            .take()
    }

    fn log_progress(&self, record: Map<String, Value>) -> Result<(), String> {
        let (session_id, course) = {
            let guard = self.session.lock().expect("session lock");
            let state = &guard.data.state;
            (
                state.id.clone(),
                state.config.get("course").cloned().unwrap_or(Value::Null),
            )
        };
        // Node's `{...rec, at, sessionId, course}`: the extra keys land last, in this order.
        let mut full = record;
        full.insert("at".to_string(), json!(events::iso_now()));
        full.insert("sessionId".to_string(), json!(session_id));
        if !course.is_null() {
            full.insert("course".to_string(), course);
        }
        knowledge::append_progress(&self.shared.opts.data_root, &Value::Object(full))
            .map_err(|error| error.to_string())
    }

    fn start_job(&self, kind: &str, input: Value, title: &str) -> Result<String, String> {
        self.shared.opts.jobs.start(kind, input, title, &self.id)
    }

    fn start_map(&self, target: &str) -> Result<(), String> {
        self.shared.opts.jobs.start_map(target, &self.id)
    }

    fn run_config(&self) -> crate::exercises::RunConfig {
        crate::exercises::default_run_config(self.shared.opts.npm_bin.as_deref())
    }

    fn installs(&self) -> crate::exercises::Installs {
        self.shared.opts.installs.clone()
    }

    fn anki_url(&self) -> Option<String> {
        Some(self.shared.opts.anki.url().to_string())
    }

    fn semantic(&self) -> Arc<crate::semantic::SemanticSearch> {
        self.shared.opts.semantic.clone()
    }

    fn courses_changed(&self) {
        tools::board::courses_changed(&self.shared.opts.data_root, &self.shared.opts.events);
    }
}

fn flush(shared: &Shared, data: &mut SessionData, turns: &Turns) {
    data.save_pending = false;
    if data.deleted {
        return;
    }
    let dir = shared.session_dir(&data.state.id);
    if let Err(error) = fs::create_dir_all(&dir) {
        eprintln!("[sessions] save failed: {error}");
        return;
    }
    let file = StateFile {
        state: &data.state,
        provider: &data.provider,
        resume: turns.resume.as_deref(),
        pending_start: data.pending_start.as_deref(),
        last_turn: turns.saved_last_turn(),
        can_retry: turns.can_retry(),
    };
    let body = match serde_json::to_string(&file) {
        Ok(body) => body,
        Err(error) => {
            eprintln!("[sessions] save failed: {error}");
            return;
        }
    };
    if let Err(error) = js::write_atomic(&dir.join("state.json"), body.as_bytes()) {
        eprintln!("[sessions] save failed: {error}");
    }
}

fn make_session(
    shared: &Arc<Shared>,
    mut state: SessionState,
    provider: String,
    resume: Option<String>,
    last_turn: Option<LastTurn>,
    can_retry: bool,
) -> Arc<Mutex<Session>> {
    let turns = Turns::new(resume, last_turn, can_retry);
    state.retryable = turns.can_retry();
    let data = SessionData {
        state,
        provider,
        pending_start: None,
        pending: IndexMap::new(),
        subs: Vec::new(),
        closing: false,
        streamed: HashMap::new(),
        finalised: HashMap::new(),
        save_pending: false,
        deleted: false,
        open_problem: None,
        manager: Arc::downgrade(shared),
    };
    Arc::new(Mutex::new(Session { data, turns }))
}

/// A `progress.jsonl` record belongs to this lesson when its slug matches.
fn lesson_progress(records: Vec<Value>, lesson_slug: &str) -> Vec<Value> {
    records
        .into_iter()
        .filter(|record| record.get("lessonSlug").and_then(Value::as_str) == Some(lesson_slug))
        .collect()
}

impl SessionManager {
    /// Runs `load`: blocking, so callers with a runtime use `spawn_blocking`.
    pub fn new(opts: ManagerOptions) -> Self {
        let shared = Arc::new(Shared {
            opts,
            sessions: Mutex::new(IndexMap::new()),
            sessions_touch_pending: AtomicBool::new(false),
        });
        let manager = Self(shared);
        manager.load();
        manager
    }

    // ───────────── persistence ─────────────

    fn load(&self) {
        let root = self.0.sessions_root();
        let Ok(entries) = fs::read_dir(&root) else {
            return;
        };
        for entry in entries.flatten() {
            let dir_id = entry.file_name().to_string_lossy().into_owned();
            let Ok(raw_text) = fs::read_to_string(entry.path().join("state.json")) else {
                continue;
            };
            let Ok(raw) = serde_json::from_str::<Value>(&raw_text) else {
                continue;
            };
            let Some(state_value) = raw.get("state") else {
                continue;
            };
            let Ok(mut state) = serde_json::from_value::<SessionState>(state_value.clone()) else {
                continue;
            };
            let mut can_retry = raw
                .get("canRetry")
                .and_then(Value::as_bool)
                .unwrap_or(state.retryable);
            let last_turn = raw
                .get("lastTurn")
                .and_then(|value| serde_json::from_value::<LastTurn>(value.clone()).ok());
            let had_last_turn = raw.get("lastTurn").is_some_and(|value| !value.is_null());
            if state.status.is_busy() {
                if had_last_turn {
                    state.status = SessionStatus::Error;
                    state.last_error =
                        Some("Sessione interrotta prima del completamento del turno".to_string());
                    can_retry = true;
                } else {
                    state.status = SessionStatus::Idle;
                }
            }
            state.pending_request_id = None;
            for item in &mut state.items {
                if item.get("kind").and_then(Value::as_str) == Some("job") {
                    // The job manager is the source of truth; a card left "running" by a previous
                    // run is an interrupted job only when it has no record.
                    let record = item
                        .get("job")
                        .and_then(|job| job.get("id"))
                        .and_then(Value::as_str)
                        .and_then(|id| self.0.opts.jobs.get(id));
                    if let Some(record) = record {
                        item["job"] = record;
                    } else {
                        let job = item
                            .get_mut("job")
                            .filter(|job| {
                                job.get("status").and_then(Value::as_str) == Some("running")
                            })
                            .and_then(Value::as_object_mut);
                        if let Some(job) = job {
                            job.insert("status".to_string(), json!("error"));
                            job.insert(
                                "error".to_string(),
                                json!("interrotto alla chiusura dell'app"),
                            );
                            job.insert("activity".to_string(), json!(""));
                        }
                    }
                }
                if item.get("kind").and_then(Value::as_str) == Some("teacher") {
                    item["streaming"] = Value::Bool(false);
                }
                let interactive = item.get("requestId").is_some();
                let settled = ["result", "answer", "decision"]
                    .iter()
                    .any(|key| item.get(key).is_some_and(|value| js::truthy(Some(value))));
                if interactive && !settled {
                    item["cancelled"] = Value::Bool(true);
                }
            }
            // Files written before the engine was pinned / the token was adapter-owned: claude, claudeSessionId once it ran.
            let provider = raw
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("claude")
                .to_string();
            let resume = raw
                .get("resume")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    if js::truthy(raw.get("hasRun")) {
                        raw.get("claudeSessionId")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    } else {
                        None
                    }
                });
            let pending_start = raw
                .get("pendingStart")
                .and_then(Value::as_str)
                .map(str::to_string);
            let session = make_session(&self.0, state, provider, resume, last_turn, can_retry);
            let reask = {
                let mut guard = session.lock().expect("session lock");
                let data = &mut guard.data;
                if data.state.status != SessionStatus::Closed {
                    data.pending_start = pending_start.clone();
                }
                let teach = data.state.config.get("kind").and_then(Value::as_str) == Some("teach");
                let anki_decided =
                    matches!(data.state.config.get("ankiOptIn"), Some(Value::Bool(_)));
                if data.state.status != SessionStatus::Closed
                    && pending_start.is_some()
                    && teach
                    && !anki_decided
                {
                    // The card was cancelled by the reload: ask again instead of leaving a session that can never start well.
                    data.state.items.retain(|item| {
                        !(item.get("kind").and_then(Value::as_str) == Some("ask")
                            && item
                                .get("input")
                                .and_then(|input| input.get("question"))
                                .and_then(Value::as_str)
                                == Some(ANKI_OPT_IN_QUESTION)
                            && item.get("cancelled").and_then(Value::as_bool) == Some(true))
                    });
                    true
                } else {
                    false
                }
            };
            self.0
                .sessions
                .lock()
                .expect("sessions lock")
                .insert(dir_id, session.clone());
            if reask {
                self.ask_anki_opt_in(&session);
            }
        }
    }

    // ───────────── public API ─────────────

    /// A job owned by a session changed: upsert its card; a result now owed goes to an idle teacher
    /// at once (`sessions.ts:382-387`).
    pub fn on_job(&self, job: Value) {
        let Some(session_id) = job.get("sessionId").and_then(Value::as_str) else {
            return;
        };
        let Some(session) = self.0.session(session_id) else {
            return;
        };
        {
            let mut guard = session.lock().expect("session lock");
            if guard.data.state.status == SessionStatus::Closed {
                return;
            }
            let mut card = Map::new();
            card.insert(
                "id".to_string(),
                json!(format!(
                    "job:{}",
                    job.get("id").and_then(Value::as_str).unwrap_or_default()
                )),
            );
            card.insert(
                "at".to_string(),
                job.get("createdAt").cloned().unwrap_or(Value::Null),
            );
            card.insert("kind".to_string(), json!("job"));
            card.insert("job".to_string(), job.clone());
            guard.data.upsert(Value::Object(card));
        }
        if crate::turns::is_owed(&job) {
            self.deliver(&session);
        }
    }

    /// Hands owed results to an idle teacher, relaunching the engine if needed (`turns.ts:155-158`).
    fn deliver(&self, session: &Arc<Mutex<Session>>) {
        let needs_launch = {
            let mut guard = session.lock().expect("session lock");
            let Session { data, turns } = &mut *guard;
            if !turns.should_deliver(&*data) {
                return;
            }
            let needs_launch = turns.begin_send(data, None, Vec::new(), None);
            if !needs_launch {
                turns.finish_send(data);
            }
            needs_launch
        };
        if needs_launch {
            // A failed launch already set status "error": the results stay owed and ride along.
            self.spawn_turn(session.clone());
        }
    }

    /// `launch_turn` off the caller's stack: the callback that starts a delivery may run anywhere.
    fn spawn_turn(&self, session: Arc<Mutex<Session>>) {
        let manager = self.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let _ = manager.launch_turn(&session).await;
                });
            }
            Err(_) => {
                std::thread::spawn(move || {
                    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    else {
                        return;
                    };
                    runtime.block_on(async move {
                        let _ = manager.launch_turn(&session).await;
                    });
                });
            }
        }
    }

    pub fn list(&self) -> Value {
        let sessions: Vec<Arc<Mutex<Session>>> = self
            .0
            .sessions
            .lock()
            .expect("sessions lock")
            .values()
            .cloned()
            .collect();
        let mut rows: Vec<Value> = sessions
            .iter()
            .map(|session| {
                let guard = session.lock().expect("session lock");
                session_summary(&guard.data.state)
            })
            .collect();
        rows.sort_by(|a, b| {
            let left = b.get("createdAt").and_then(Value::as_str).unwrap_or("");
            let right = a.get("createdAt").and_then(Value::as_str).unwrap_or("");
            left.cmp(right)
        });
        Value::Array(rows)
    }

    pub fn get(&self, id: &str) -> Option<Value> {
        let session = self.0.session(id)?;
        let guard = session.lock().expect("session lock");
        serde_json::to_value(&guard.data.state).ok()
    }

    /// Sends the full state first, then every live event; the channel is the SSE stream.
    pub fn subscribe(&self, id: &str) -> Option<UnboundedReceiver<String>> {
        let session = self.0.session(id)?;
        let mut guard = session.lock().expect("session lock");
        let (sender, receiver) = mpsc::unbounded_channel();
        let snapshot = ServerEvent::Snapshot {
            state: guard.data.state.clone(),
        };
        let payload = serde_json::to_string(&snapshot).ok()?;
        let _ = sender.send(payload);
        guard.data.subs.push(sender);
        Some(receiver)
    }

    pub async fn create(&self, body: Value) -> Result<String, HttpError> {
        let mut config = validate_config(&body).map_err(|message| HttpError::new(400, message))?;
        let settings = settings::read_settings(&self.0.opts.repo_root);
        if !config.contains_key("model") {
            // `config.model ??= settings.teacherModel`: a new key appends, like the JS assignment.
            config.insert(
                "model".to_string(),
                Value::String(settings.teacher_model.clone()),
            );
        }
        let root = self.0.opts.data_root.clone();
        let course = config
            .get("course")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let kind = config
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let course_dir = root.join("courses").join(&course);

        if course == courses::STANDALONE_COURSE_SLUG {
            courses::ensure_standalone_course(&root, true)
                .map_err(|error| HttpError::new(500, error.to_string()))?;
        }
        if kind != "plan" && !course_dir.exists() {
            return Err(HttpError::new(
                400,
                format!("Course \"{course}\" does not exist"),
            ));
        }
        if kind == "teach" {
            let imports = self.0.opts.imports.clone();
            let anki = self.0.opts.anki.clone();
            let data_root = root.clone();
            let import_course = course.clone();
            tokio::spawn(async move {
                // Best effort: an offline Anki never blocks a lesson.
                let _ = imports.import(anki, data_root, import_course).await;
            });
        }

        let mut lesson_path: Option<String> = None;
        let mut restored: Option<(String, String, String, usize)> = None;
        let title: String;
        if kind == "plan" {
            let Some(chapter_title) = config.get("chapterTitle").and_then(Value::as_str) else {
                return Err(HttpError::new(
                    400,
                    "chapterTitle is required for plan sessions",
                ));
            };
            title = format!("Piano · {chapter_title}");
        } else if kind == "practice" {
            title = format!("Esercizi · {course}");
        } else {
            let Some(target) = config.get("target").and_then(Value::as_object) else {
                return Err(HttpError::new(400, "target is required for teach sessions"));
            };
            match target.get("mode").and_then(Value::as_str) {
                Some("planned") => {
                    let planned_title = target
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let chapters =
                        courses::read_curriculum(&root).map_err(|error| match error {
                            CoursesError::Syntax => HttpError::new(400, "Invalid JSON body"),
                            CoursesError::Other(message) => HttpError::new(500, message),
                        })?;
                    let planned = chapters
                        .iter()
                        .find(|chapter| js::js_string(chapter.slug.as_ref()) == course)
                        .is_some_and(|chapter| {
                            chapter.lessons.iter().any(|lesson| {
                                lesson.get("title").and_then(Value::as_str)
                                    == Some(planned_title.as_str())
                            })
                        });
                    if !planned {
                        return Err(HttpError::new(
                            400,
                            format!("\"{planned_title}\" is not a planned lesson of {course}"),
                        ));
                    }
                    let slug = slug::slugify(&planned_title);
                    let lessons_dir = course_dir.join("lessons");
                    let already_taught = fs::read_dir(&lessons_dir)
                        .map(|entries| {
                            entries.flatten().any(|entry| {
                                let name = entry.file_name().to_string_lossy().into_owned();
                                name.ends_with(".html") && lesson::slug_of_lesson(&name) == slug
                            })
                        })
                        .unwrap_or(false);
                    if already_taught {
                        return Err(HttpError::new(
                            400,
                            format!(
                                "\"{planned_title}\" è già stata svolta: usa \"Continua\" sulla lezione esistente."
                            ),
                        ));
                    }
                    title = planned_title;
                }
                Some("existing") => {
                    let raw_path = target
                        .get("lessonPath")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let prefix = format!("courses/{course}/lessons/");
                    let rel = lesson::lesson_rel(raw_path)
                        .filter(|rel| rel.starts_with(&prefix) && root.join(rel).exists());
                    let Some(rel) = rel else {
                        return Err(HttpError::new(
                            400,
                            format!("lessonPath must be an existing file under {prefix}"),
                        ));
                    };
                    let lesson = lesson::read_lesson(&root, &rel)
                        .map_err(|message| HttpError::new(400, message))?
                        .ok_or_else(|| {
                            HttpError::new(
                                400,
                                "That lesson has no data island and cannot be continued",
                            )
                        })?;
                    let doc = &lesson.doc;
                    let nodes = doc
                        .get("nodes")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    let metadata = doc.get("metadata");
                    let initial_dag = metadata
                        .and_then(|metadata| metadata.get("initialDag"))
                        .and_then(Value::as_str);
                    if nodes == 0 && initial_dag.is_none() {
                        return Err(HttpError::new(
                            400,
                            "Questa lezione è in prosa (scritta prima del motore, senza nodi né piano): non si può continuare. Scegli una lezione del piano o una lezione libera.",
                        ));
                    }
                    title = metadata
                        .and_then(|metadata| metadata.get("title"))
                        .map(|title| js::js_string(Some(title)))
                        .unwrap_or_default();
                    lesson_path = Some(rel);
                    if let Some(dag) = initial_dag {
                        // The plan is already approved in the artifact: restore it so the Piano tab and quiz nodeId checks work.
                        let topic = metadata
                            .and_then(|metadata| metadata.get("topic"))
                            .map(|topic| js::js_string(Some(topic)))
                            .unwrap_or_default();
                        let goal = metadata
                            .and_then(|metadata| metadata.get("targetGoal"))
                            .map(|goal| js::js_string(Some(goal)))
                            .unwrap_or_default();
                        restored = Some((topic, goal, dag.to_string(), nodes));
                    }
                }
                _ => {
                    title = if course == courses::STANDALONE_COURSE_SLUG {
                        let note = config
                            .get("note")
                            .and_then(Value::as_str)
                            .map(js::js_trim)
                            .unwrap_or("");
                        if note.is_empty() {
                            "Lezione singola".to_string()
                        } else {
                            format!("Lezione singola · {}", trunc(note, 60))
                        }
                    } else {
                        format!("Lezione libera · {course}")
                    };
                }
            }
        }

        let id = engine::uuid_v4();
        let now = events::iso_now();
        let plan = match &restored {
            Some((topic, goal, dag, node_count)) => {
                let slug = lesson::slug_of_lesson(lesson_path.as_deref().unwrap_or(""));
                let progress = lesson_progress(knowledge::read_progress(&root, &course), &slug);
                planstate::derive_plan_state(topic, goal, dag, true, *node_count, &progress)
            }
            None => Value::Null,
        };
        let state = SessionState {
            id: id.clone(),
            config,
            title: title.clone(),
            created_at: now.clone(),
            updated_at: now,
            status: SessionStatus::Starting,
            lesson_path: lesson_path.clone(),
            usage: Usage::default(),
            items: Vec::new(),
            pending_request_id: None,
            plan,
            exercises: Vec::new(),
            retryable: false,
            last_error: None,
        };
        let session = make_session(&self.0, state, settings.provider.clone(), None, None, false);
        self.0
            .sessions
            .lock()
            .expect("sessions lock")
            .insert(id.clone(), session.clone());
        self.0.touch_sessions();
        {
            let mut guard = session.lock().expect("session lock");
            guard.data.pending_start = Some(title.clone());
            let Session { data, turns } = &mut *guard;
            flush(&self.0, data, turns);
        }
        let teach = kind == "teach";
        let anki_decided = {
            let guard = session.lock().expect("session lock");
            matches!(
                guard.data.state.config.get("ankiOptIn"),
                Some(Value::Bool(_))
            )
        };
        if teach && !anki_decided {
            self.ask_anki_opt_in(&session);
            return Ok(id);
        }
        self.start(&session, &title, None, None)
            .await
            .map_err(|message| HttpError::new(500, message))?;
        Ok(id)
    }

    /// Teach session without an explicit opt-in: ask first, with no claude process until the answer arrives.
    fn ask_anki_opt_in(&self, session: &Arc<Mutex<Session>>) {
        let request_id = engine::uuid_v4();
        let item = json!({
            "id": format!("req:{request_id}"),
            "at": events::iso_now(),
            "kind": "ask",
            "requestId": request_id,
            "input": {
                "question": ANKI_OPT_IN_QUESTION,
                "options": [
                    {"label": "Sì", "value": "si"},
                    {"label": "No", "value": "no"},
                ],
            },
        });
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut guard = session.lock().expect("session lock");
        let Session { data, turns } = &mut *guard;
        data.pending.insert(
            request_id.clone(),
            PendingReq {
                item_id,
                kind: "ask".to_string(),
                resolver: Resolver::AnkiOptIn,
            },
        );
        data.upsert(item);
        data.set_pending();
        let retryable = turns.can_retry();
        data.apply_status(SessionStatus::Waiting, None, retryable);
    }

    /// The opening turn (launches the engine). Failure: status "error", pendingStart kept, the error out.
    async fn start(
        &self,
        session: &Arc<Mutex<Session>>,
        title: &str,
        learner_text: Option<String>,
        attachments: Option<Value>,
    ) -> Result<(), String> {
        let needs_launch = {
            let mut guard = session.lock().expect("session lock");
            let Session { data, turns } = &mut *guard;
            let opening = context::opening_message(&data.state.config, Some(title));
            // One turn: the opening, then what the learner typed (the process has to see both, in this order).
            let (text, show) = match learner_text {
                Some(learner_text) => (
                    format!("{opening}\n\nMessaggio dello studente:\n\n{learner_text}"),
                    vec![opening, learner_text],
                ),
                None => (opening.clone(), vec![opening]),
            };
            let needs_launch = turns.begin_send(data, Some(text), show, attachments);
            if !needs_launch {
                turns.finish_send(data);
            }
            needs_launch
        };
        if needs_launch {
            self.launch_turn(session).await?;
        }
        let mut guard = session.lock().expect("session lock");
        let Session { data, turns } = &mut *guard;
        data.pending_start = None;
        flush(&self.0, data, turns);
        Ok(())
    }

    /// Message body checks first, then the id lookup (Node's `textBody` runs before `manager.message`).
    pub async fn message(&self, id: &str, body: Value) -> Result<(), HttpError> {
        let (text, attachments) = validate_message(&body)?;
        let Some(session) = self.0.session(id) else {
            return Err(HttpError::new(404, "Session not found"));
        };
        let (needs_launch, opening) = self.begin_message(&session, text, attachments)?;
        if needs_launch {
            self.launch_turn(&session)
                .await
                .map_err(|message| HttpError::new(500, message))?;
        }
        if opening {
            let mut guard = session.lock().expect("session lock");
            let Session { data, turns } = &mut *guard;
            data.pending_start = None;
            flush(&self.0, data, turns);
        }
        Ok(())
    }

    /// The shared secret the engine's `mcp.json` carries.
    pub fn token(&self) -> String {
        self.0.opts.token.clone()
    }

    pub fn session_config(&self, id: &str) -> Option<Map<String, Value>> {
        let session = self.0.session(id)?;
        let guard = session.lock().expect("session lock");
        Some(guard.data.state.config.clone())
    }

    /// The MCP route for one session: `None` when the id is unknown (Node's `getContext`).
    pub fn tool_host(&self, id: &str, call: CallRequests) -> Option<SessionToolHost> {
        let session = self.0.session(id)?;
        let id = session.lock().expect("session lock").data.state.id.clone();
        Some(SessionToolHost {
            shared: self.0.clone(),
            session,
            call,
            id,
        })
    }

    /// The `/mcp` call was dropped: only the cards it created can never be answered.
    pub fn cancel_requests(&self, id: &str, only: &[String]) {
        let Some(session) = self.0.session(id) else {
            return;
        };
        session
            .lock()
            .expect("session lock")
            .data
            .cancel_pending_only(Some(only));
    }

    /// Port of Node's `answer` (`sessions.ts:577-601`): the card settles and its caller resumes.
    pub fn answer(&self, id: &str, body: Value) -> Result<(), HttpError> {
        // Node parses the body before it looks the session up.
        let body = validate_answer(&body)?;
        let Some(session) = self.0.session(id) else {
            return Err(HttpError::new(404, "Session not found"));
        };
        let kind = body
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let payload = body.get("answer").cloned().unwrap_or(Value::Null);
        let mut request_id = body
            .get("requestId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let resolver = {
            let mut guard = session.lock().expect("session lock");
            let found = guard
                .data
                .pending
                .get(&request_id)
                .map(|pending| (pending.kind.clone(), pending.item_id.clone()));
            let (pending_kind, item_id) = match found {
                Some(found) => found,
                None => {
                    // Superseded by a retry: the answer goes to the active card of the same kind.
                    match guard.data.pending.iter().next() {
                        Some((key, first)) if first.kind == kind => {
                            request_id = key.clone();
                            (first.kind.clone(), first.item_id.clone())
                        }
                        _ => return Err(HttpError::new(409, "No pending request with that id")),
                    }
                }
            };
            if pending_kind != kind {
                return Err(HttpError::new(
                    400,
                    format!("Request is a {pending_kind}, not a {kind}"),
                ));
            }
            if let Some(index) = guard.data.item_index(&item_id) {
                let item = &mut guard.data.state.items[index];
                match kind.as_str() {
                    "quiz" => {
                        let input = item.get("input").cloned().unwrap_or(Value::Null);
                        item["result"] = tools::util::grade_quiz(&input, &payload);
                    }
                    "ask" => item["answer"] = payload.clone(),
                    _ => item["decision"] = payload.clone(),
                }
                let item = item.clone();
                guard.data.notify(ServerEvent::Item { item });
            }
            let pending = guard.data.pending.shift_remove(&request_id);
            guard.data.set_pending();
            if guard.data.pending.is_empty() {
                let retryable = guard.turns.can_retry();
                guard
                    .data
                    .apply_status(SessionStatus::Running, None, retryable);
            }
            pending.map(|pending| pending.resolver)
        };
        match resolver {
            Some(Resolver::Tool(tx)) => {
                let _ = tx.send(Ok(payload));
            }
            Some(Resolver::AnkiOptIn) => {
                // Node's `.then`: store the opt-in, flush, then send the opening turn.
                let opt_in = payload
                    .get("selected")
                    .and_then(Value::as_array)
                    .and_then(|selected| selected.first())
                    .and_then(Value::as_str)
                    == Some("si");
                let title = {
                    let mut guard = session.lock().expect("session lock");
                    guard
                        .data
                        .state
                        .config
                        .insert("ankiOptIn".to_string(), json!(opt_in));
                    let Session { data, turns } = &mut *guard;
                    flush(&self.0, data, turns);
                    data.pending_start.clone()
                };
                if let Some(title) = title {
                    let manager = self.clone();
                    let session = session.clone();
                    tokio::spawn(async move {
                        // A failed start already set status "error": nothing left to report.
                        let _ = manager.start(&session, &title, None, None).await;
                    });
                }
            }
            None => {}
        }
        Ok(())
    }

    /// The closed/busy checks, the `pendingStart` decision and the turn under one lock: Node checks
    /// the status and marks the session busy in the same synchronous tick, so a second concurrent
    /// message gets 409. Returns `(needs_launch, opening)`.
    fn begin_message(
        &self,
        session: &Arc<Mutex<Session>>,
        text: String,
        attachments: Option<Value>,
    ) -> Result<(bool, bool), HttpError> {
        let mut guard = session.lock().expect("session lock");
        let Session { data, turns } = &mut *guard;
        if data.state.status == SessionStatus::Closed {
            return Err(HttpError::new(409, "Session is closed"));
        }
        if data.state.status.is_busy() {
            return Err(HttpError::new(
                409,
                "The teacher is busy: wait for the current turn to finish",
            ));
        }
        let opening = data.pending_start.clone().map(|title| {
            // Never ran (Anki card dismissed, start failed, app restarted): open the lesson, with his message in the same turn.
            let opening = context::opening_message(&data.state.config, Some(&title));
            // No answer to the Anki question means no opt-in: cards are never created by default.
            if data.state.config.get("kind").and_then(Value::as_str) == Some("teach")
                && !matches!(data.state.config.get("ankiOptIn"), Some(Value::Bool(_)))
            {
                data.state
                    .config
                    .insert("ankiOptIn".to_string(), Value::Bool(false));
            }
            opening
        });
        let (turn_text, show) = match &opening {
            Some(opening) => (
                format!("{opening}\n\nMessaggio dello studente:\n\n{text}"),
                vec![opening.clone(), text],
            ),
            None => (text.clone(), vec![text]),
        };
        let needs_launch = turns.begin_send(data, Some(turn_text), show, attachments);
        if !needs_launch {
            turns.finish_send(data);
        }
        Ok((needs_launch, opening.is_some()))
    }

    /// The async half of a turn: start the engine, attach it, push the learner bubbles, mark running.
    async fn launch_turn(&self, session: &Arc<Mutex<Session>>) -> Result<(), String> {
        let id = session.lock().expect("session lock").data.state.id.clone();
        let (handle, receiver) = match self.launch(session).await {
            Ok(started) => started,
            Err(message) => {
                let mut guard = session.lock().expect("session lock");
                let Session { data, turns } = &mut *guard;
                turns.launch_failed(data, &message);
                return Err(message);
            }
        };
        let mut stop_after = None;
        {
            let mut guard = session.lock().expect("session lock");
            let Session { data, turns } = &mut *guard;
            match turns.attach(handle) {
                Ok(generation) => {
                    tokio::spawn(self.clone().pump(id, generation, receiver));
                    turns.finish_send(data);
                }
                Err(handle) => stop_after = Some(handle),
            }
        }
        if let Some(mut handle) = stop_after {
            // The session closed while the engine was starting: the turn never goes out.
            let message = "Session is closed".to_string();
            tokio::spawn(async move { handle.stop().await });
            let mut guard = session.lock().expect("session lock");
            let Session { data, turns } = &mut *guard;
            turns.launch_failed(data, &message);
            return Err(message);
        }
        Ok(())
    }

    /// Starts this session's pinned engine (TurnHost.spawn).
    async fn launch(
        &self,
        session: &Arc<Mutex<Session>>,
    ) -> Result<(Box<dyn EngineHandle>, UnboundedReceiver<EngineMsg>), String> {
        let opts = &self.0.opts;
        let (id, config, lesson_path, provider, resume) = {
            let guard = session.lock().expect("session lock");
            let data = &guard.data;
            (
                data.state.id.clone(),
                data.state.config.clone(),
                data.state.lesson_path.clone(),
                data.provider.clone(),
                guard.turns.resume.clone(),
            )
        };
        let today = events::iso_now();
        let today = today[..10].to_string();
        let prompt_config = config.clone();
        let prompt = {
            let code_root = opts.code_root.clone();
            let repo_root = opts.repo_root.clone();
            let data_root = opts.data_root.clone();
            tokio::task::spawn_blocking(move || {
                context::build_system_prompt(&PromptInput {
                    config: &prompt_config,
                    code_root: &code_root,
                    repo_root: &repo_root,
                    data_root: Some(&data_root),
                    lesson_path: lesson_path.as_deref(),
                    today: &today,
                })
            })
            .await
            .map_err(|error| error.to_string())?
        };
        if session.lock().expect("session lock").data.closing {
            return Err("Session is closed".to_string());
        }
        let settings = settings::read_settings(&opts.repo_root);
        let alias = config.get("model").and_then(Value::as_str);
        let model = engine::model_for(&settings, &provider, "teacher", alias);
        let effort = engine::effort_for(&settings, &provider, "teacher");
        let grant = vec![Grant::Read, Grant::Web];
        let system_prompt = engine::with_engine_note(&prompt, &provider, &grant);
        let bin = match provider.as_str() {
            "agy" => opts.agy.clone(),
            "opencode" => opts.opencode.clone(),
            _ => opts.claude.clone(),
        };
        let on_limits = {
            let repo_root = opts.repo_root.clone();
            let cache = opts.limits.clone();
            let events = opts.events.clone();
            Arc::new(move |value: Value| {
                limits::record_limits(&repo_root, &cache, &events, &value);
            })
        };
        let launch = ClaudeLaunch {
            bin,
            repo_root: opts.repo_root.clone(),
            data_root: Some(opts.data_root.clone()),
            scratch_dir: self.0.session_dir(&id),
            model,
            effort,
            system_prompt,
            grant,
            mcp: McpServer {
                name: "app".to_string(),
                url: format!("http://127.0.0.1:{}/mcp/{}", opts.port, id),
                token: opts.token.clone(),
            },
            add_dirs: opts.add_dirs.clone(),
            max_turns: None,
            resume,
            settings,
            on_limits,
        };
        Ok(match provider.as_str() {
            "agy" => agy::spawn_agy(launch),
            "opencode" => opencode::spawn_opencode(launch),
            _ => claude::spawn_claude(launch),
        })
    }

    async fn pump(
        self,
        id: String,
        mut generation: u64,
        mut receiver: UnboundedReceiver<EngineMsg>,
    ) {
        loop {
            let mut relaunched = None;
            while let Some(message) = receiver.recv().await {
                let Some(session) = self.0.session(&id) else {
                    return;
                };
                let action = {
                    let mut guard = session.lock().expect("session lock");
                    let Session { data, turns } = &mut *guard;
                    match message {
                        EngineMsg::Event(event) => turns.on_event(data, generation, &event),
                        EngineMsg::Stderr(line) => {
                            turns.on_stderr(generation, &line);
                            TurnAction::None
                        }
                        EngineMsg::Exit { code, signal } => {
                            turns.on_exit(data, generation, code, signal)
                        }
                    }
                };
                match action {
                    TurnAction::None => {}
                    TurnAction::Stop(mut handle) => {
                        tokio::spawn(async move { handle.stop().await });
                    }
                    TurnAction::RelaunchFresh(text) => {
                        relaunched = self.relaunch_fresh(&id, &text).await;
                        break;
                    }
                    TurnAction::Deliver => self.deliver(&session),
                }
            }
            match relaunched {
                Some((next_generation, next_receiver)) => {
                    generation = next_generation;
                    receiver = next_receiver;
                }
                None => return,
            }
        }
    }

    /// A fresh engine after a refused resume: the caller continues pumping the returned channel.
    async fn relaunch_fresh(
        &self,
        id: &str,
        text: &str,
    ) -> Option<(u64, UnboundedReceiver<EngineMsg>)> {
        let session = self.0.session(id)?;
        let (handle, receiver) = match self.launch(&session).await {
            Ok(started) => started,
            Err(message) => {
                let mut guard = session.lock().expect("session lock");
                let Session { data, turns } = &mut *guard;
                turns.relaunch_failed(data, &message);
                return None;
            }
        };
        let mut stopped_handle = None;
        let attached = {
            let mut guard = session.lock().expect("session lock");
            let Session { data, turns } = &mut *guard;
            match turns.attach(handle) {
                Ok(generation) => {
                    turns.resend(data, text);
                    Some(generation)
                }
                Err(handle) => {
                    stopped_handle = Some(handle);
                    None
                }
            }
        };
        if let Some(mut handle) = stopped_handle {
            tokio::spawn(async move { handle.stop().await });
            let mut guard = session.lock().expect("session lock");
            let Session { data, turns } = &mut *guard;
            turns.relaunch_failed(data, "Session is closed");
            return None;
        }
        attached.map(|generation| (generation, receiver))
    }

    // ───────────── session control ─────────────

    /// Node's `saveAttachments`: the files land under the session, the metadata keeps Node's order.
    pub fn save_attachments(
        &self,
        id: &str,
        files: Vec<AttachmentUpload>,
    ) -> Result<Value, HttpError> {
        let Some(session) = self.0.session(id) else {
            return Err(HttpError::new(404, "Session not found"));
        };
        let closed = session.lock().expect("session lock").data.state.status == SessionStatus::Closed;
        if closed {
            return Err(HttpError::new(409, "Session is closed"));
        }
        let dir = self.0.session_dir(id).join("attachments");
        fs::create_dir_all(&dir).map_err(|error| HttpError::new(500, error.to_string()))?;
        let mut metas = Vec::new();
        for file in files {
            if file.name.is_empty() {
                continue;
            }
            let raw = attachment_base(&file.name);
            let safe = safe_attachment_name(raw);
            let ext = attachment_ext(&safe);
            if !ALLOWED_ATTACHMENT_EXTS.contains(&ext.as_str()) {
                // The files already written stay, as in Node.
                let shown = if ext.is_empty() {
                    "sconosciuto".to_string()
                } else {
                    ext
                };
                return Err(HttpError::new(
                    400,
                    format!("Tipo di file non supportato: {shown}"),
                ));
            }
            let file_id = engine::uuid_v4();
            let stored = format!("{}_{}", &file_id[..8], safe);
            fs::write(dir.join(&stored), &file.bytes)
                .map_err(|error| HttpError::new(500, error.to_string()))?;
            let mime_type = file
                .mime_type
                .filter(|mime| !mime.is_empty())
                .map(|mime| mime.to_ascii_lowercase())
                .unwrap_or_else(|| "application/octet-stream".to_string());
            let mut meta = Map::new();
            meta.insert("id".to_string(), json!(file_id));
            meta.insert("name".to_string(), json!(raw));
            meta.insert(
                "path".to_string(),
                json!(format!(".learner/sessions/{id}/attachments/{stored}")),
            );
            meta.insert("mimeType".to_string(), json!(mime_type));
            meta.insert("size".to_string(), json!(file.bytes.len()));
            metas.push(Value::Object(meta));
        }
        Ok(Value::Array(metas))
    }

    /// Node's `interrupt`: in-band for Claude, so the process stays up. `Some(ticket)` arms the watchdog.
    pub fn interrupt(&self, id: &str) -> Result<(), HttpError> {
        let Some(session) = self.0.session(id) else {
            return Err(HttpError::new(404, "Session not found"));
        };
        let ticket = {
            let mut guard = session.lock().expect("session lock");
            let Session { data, turns } = &mut *guard;
            turns.interrupt(data)
        };
        if let Some(ticket) = ticket {
            let weak = Arc::downgrade(&self.0);
            let id = id.to_string();
            spawn_delayed(crate::turns::WATCHDOG, move || {
                if let Some(shared) = weak.upgrade() {
                    shared.watchdog_fired(&id, ticket);
                }
            });
        }
        Ok(())
    }

    /// Node's `retry`: reruns the saved turn. Checks, trim and turn under one lock (rules.md).
    pub async fn retry(&self, id: &str) -> Result<(), HttpError> {
        let Some(session) = self.0.session(id) else {
            return Err(HttpError::new(404, "Session not found"));
        };
        let needs_launch = {
            let mut guard = session.lock().expect("session lock");
            let Session { data, turns } = &mut *guard;
            if data.state.status == SessionStatus::Closed {
                return Err(HttpError::new(409, "Session is closed"));
            }
            if data.state.status.is_busy() {
                return Err(HttpError::new(
                    409,
                    "The teacher is busy: wait for the current turn to finish",
                ));
            }
            if !turns.can_retry() {
                return Err(HttpError::new(400, "Nessun turno fallito da riprovare"));
            }
            if turns
                .saved_last_turn()
                .is_some_and(|turn| turn.pushed_learner)
            {
                let last = data.state.items.iter().rposition(|item| {
                    item.get("kind").and_then(Value::as_str) == Some("learner")
                });
                if let Some(index) = last {
                    data.state.items.truncate(index + 1);
                }
            } else {
                data.state.items.retain(|item| {
                    let kind = item.get("kind").and_then(Value::as_str);
                    let error_notice = kind == Some("notice")
                        && item.get("level").and_then(Value::as_str) == Some("error");
                    let streaming_teacher = kind == Some("teacher")
                        && item.get("streaming").and_then(Value::as_bool) == Some(true);
                    !error_notice && !streaming_teacher
                });
            }
            data.streamed.clear();
            data.finalised.clear();
            data.state.updated_at = events::iso_now();
            let state = data.state.clone();
            data.notify(ServerEvent::Snapshot { state });
            let needs_launch = turns.begin_retry(data).map_err(|message| {
                HttpError::new(if message == "Session is closed" { 409 } else { 400 }, message)
            })?;
            if !needs_launch {
                turns.finish_send(data);
            }
            needs_launch
        };
        if needs_launch {
            self.launch_turn(&session)
                .await
                .map_err(|message| HttpError::new(500, message))?;
        }
        let mut guard = session.lock().expect("session lock");
        let Session { data, turns } = &mut *guard;
        flush(&self.0, data, turns);
        Ok(())
    }

    /// No more turns: the pending cards are cancelled and the engine is stopped.
    async fn halt(&self, session: &Arc<Mutex<Session>>) {
        let handle = {
            let mut guard = session.lock().expect("session lock");
            let Session { data, turns } = &mut *guard;
            data.closing = true;
            data.cancel_pending();
            turns.stop()
        };
        if let Some(mut handle) = handle {
            handle.stop().await;
        }
    }

    pub async fn close(&self, id: &str) -> Result<(), HttpError> {
        let Some(session) = self.0.session(id) else {
            return Err(HttpError::new(404, "Session not found"));
        };
        self.halt(&session).await;
        let mut guard = session.lock().expect("session lock");
        let Session { data, turns } = &mut *guard;
        let retryable = turns.can_retry();
        data.apply_status(SessionStatus::Closed, None, retryable);
        flush(&self.0, data, turns);
        Ok(())
    }

    /// Stops the session and forgets it: its `.learner/sessions/<id>` dir (attachments included) goes.
    pub async fn delete(&self, id: &str) -> Result<(), HttpError> {
        let Some(session) = self.0.session(id) else {
            return Err(HttpError::new(404, "Session not found"));
        };
        self.halt(&session).await;
        {
            let mut guard = session.lock().expect("session lock");
            guard.data.deleted = true;
            guard.data.save_pending = false;
        }
        self.0
            .sessions
            .lock()
            .expect("sessions lock")
            .shift_remove(id);
        let dir = self.0.session_dir(id);
        let mut failure: Option<std::io::Error> = None;
        for attempt in 0..3 {
            match fs::remove_dir_all(&dir) {
                Ok(()) => {
                    failure = None;
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    failure = None;
                    break;
                }
                Err(error) => {
                    failure = Some(error);
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        }
        self.0.touch_sessions();
        match failure {
            Some(error) => Err(HttpError::new(500, error.to_string())),
            None => Ok(()),
        }
    }

    /// The pids of the live engines, in session map order.
    pub fn live_pids(&self) -> Vec<u32> {
        let sessions: Vec<Arc<Mutex<Session>>> = self
            .0
            .sessions
            .lock()
            .expect("sessions lock")
            .values()
            .cloned()
            .collect();
        sessions
            .iter()
            .filter_map(|session| session.lock().expect("session lock").turns.pid())
            .collect()
    }

    /// Server shutdown: stop every process and persist. The retry flag is written before `stop`
    /// clears it, so a crashed turn stays retryable across a graceful quit (Node writes false here;
    /// its `state.retryable` covers the reload either way).
    pub async fn shutdown(&self) {
        let sessions: Vec<Arc<Mutex<Session>>> = self
            .0
            .sessions
            .lock()
            .expect("sessions lock")
            .values()
            .cloned()
            .collect();
        for session in sessions {
            let handle = {
                let mut guard = session.lock().expect("session lock");
                let Session { data, turns } = &mut *guard;
                data.closing = true;
                data.cancel_pending_only(None);
                flush(&self.0, data, turns);
                turns.stop()
            };
            if let Some(mut handle) = handle {
                handle.stop().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;

    use super::*;
    use crate::tools::ToolHost;

    #[test]
    fn config_validation_keeps_schema_order_and_drops_unknown_keys() {
        let config = validate_config(&json!({
            "note": "n",
            "unknown": true,
            "target": {"mode": "planned", "title": "Prima lezione", "extra": 1},
            "ankiOptIn": false,
            "course": "demo",
            "kind": "teach",
        }))
        .unwrap();
        let keys: Vec<&str> = config.keys().map(String::as_str).collect();
        assert_eq!(keys, ["kind", "course", "target", "ankiOptIn", "note"]);
        assert_eq!(
            serde_json::to_string(&config["target"]).unwrap(),
            r#"{"mode":"planned","title":"Prima lezione"}"#
        );
        assert_eq!(
            serde_json::to_string(&config["kind"]).unwrap(),
            r#""teach""#
        );
    }

    #[test]
    fn config_validation_rejects_null_and_bad_values() {
        let error = validate_config(&json!({"kind": null, "course": "demo"})).unwrap_err();
        assert!(error.contains("kind"), "{error}");
        assert!(validate_config(&json!({"kind": "teach", "course": "Demo"})).is_err());
        let error = validate_config(&json!({"kind": "teach", "course": "demo", "model": "gpt"}))
            .unwrap_err();
        assert!(error.starts_with("model: "), "{error}");
        let error = validate_config(
            &json!({"kind": "teach", "course": "demo", "target": {"mode": "planned"}}),
        )
        .unwrap_err();
        assert!(error.starts_with("target.title: "), "{error}");
        let note = "x".repeat(5001);
        let error =
            validate_config(&json!({"kind": "teach", "course": "demo", "note": note})).unwrap_err();
        assert!(error.starts_with("note: "), "{error}");
        let error = validate_config(&json!([])).unwrap_err();
        assert!(error.contains("expected object"), "{error}");
    }

    #[test]
    fn message_validation_matches_text_body() {
        for body in [
            json!({}),
            json!({"text": ""}),
            json!({"text": "x".repeat(20001)}),
            json!({"text": "x", "attachments": [{"name": "a"}]}),
            json!({"text": "x", "attachments": "nope"}),
        ] {
            let error = validate_message(&body).unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST, "{body}");
            assert!(!error.message.is_empty());
        }

        let (text, attachments) = validate_message(&json!({"text": "x".repeat(20000)})).unwrap();
        assert_eq!(text.len(), 20000);
        assert!(attachments.is_none());

        let (text, attachments) =
            validate_message(&json!({"text": "x", "attachments": []})).unwrap();
        assert_eq!(text, "x");
        assert_eq!(attachments, Some(json!([])));

        // zod's `.optional()` refuses null: a missing key is the only "no attachments".
        let error = validate_message(&json!({"text": "x", "attachments": null})).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error.message,
            "attachments: Invalid input: expected array, received null"
        );

        let error =
            validate_message(&json!({"text": "x", "attachments": [{"name": "a"}]})).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error.message,
            "attachments.0.id: Invalid input: expected string, received undefined; \
             attachments.0.path: Invalid input: expected string, received undefined; \
             attachments.0.mimeType: Invalid input: expected string, received undefined; \
             attachments.0.size: Invalid input: expected number, received undefined"
        );

        // A valid element is rebuilt in schema order, unknown keys dropped.
        let (_, attachments) = validate_message(&json!({
            "text": "x",
            "attachments": [{
                "size": 3,
                "extra": true,
                "mimeType": "m",
                "path": "p",
                "name": "n",
                "id": "i",
            }],
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_string(&attachments.expect("the metas")).unwrap(),
            r#"[{"id":"i","name":"n","path":"p","mimeType":"m","size":3}]"#
        );
    }

    #[test]
    fn attachment_name_helpers_follow_node() {
        assert_eq!(attachment_base("../../../etc/passwd.png"), "passwd.png");
        assert_eq!(attachment_base("notes.pdf/"), "notes.pdf");
        assert_eq!(attachment_base("C:foo.png"), "foo.png");
        assert_eq!(attachment_base(""), "");

        // basename, then the safe name, then Node's extname.
        let base = attachment_base("C:\\x\\a b.PDF");
        assert_eq!(base, "a b.PDF");
        let safe = safe_attachment_name(base);
        assert_eq!(safe, "a_b.PDF");
        assert_eq!(attachment_ext(&safe), ".pdf");

        assert_eq!(safe_attachment_name("😀.png"), "__.png", "one _ per UTF-16 unit");
        assert_eq!(safe_attachment_name(""), "attachment");

        assert_eq!(attachment_ext(".png"), "");
        assert_eq!(attachment_ext("..png"), ".png");
        assert_eq!(attachment_ext(".."), "");
        assert_eq!(attachment_ext("attachment"), "");
    }

    #[test]
    fn trunc_counts_utf16_units() {
        assert_eq!(trunc("abcdef", 3), "abc…");
        assert_eq!(trunc("abc", 3), "abc");
        // An emoji is two UTF-16 units, like JS's `length`.
        assert_eq!(trunc("😀😀😀", 2), "😀…");
        assert_eq!(trunc("😀😀", 4), "😀😀");
    }

    #[test]
    fn answer_validation_ports_the_node_union() {
        // Key order is what Node keeps: the union's schema order, unknown keys dropped.
        assert_eq!(
            serde_json::to_string(
                &validate_answer(&json!({
                    "answer": {"note": "n", "confidence": 5, "dontKnow": false, "selected": ["a"], "extra": 1},
                    "kind": "quiz",
                    "requestId": "r",
                }))
                .unwrap()
            )
            .unwrap(),
            r#"{"requestId":"r","kind":"quiz","answer":{"selected":["a"],"dontKnow":false,"confidence":5,"note":"n"}}"#
        );
        assert_eq!(
            validate_answer(&json!({
                "requestId": "r",
                "kind": "ask",
                "answer": {"other": "o", "selected": ["a"]},
            }))
            .unwrap(),
            json!({"requestId": "r", "kind": "ask", "answer": {"selected": ["a"], "other": "o"}})
        );
        for kind in ["plan", "curriculum"] {
            assert_eq!(
                validate_answer(&json!({
                    "requestId": "r",
                    "kind": kind,
                    "answer": {"feedback": "f", "approved": true},
                }))
                .unwrap(),
                json!({"requestId": "r", "kind": kind, "answer": {"approved": true, "feedback": "f"}})
            );
        }

        // Messages pinned from zod 4.6.7 (`main.ts:104-114`).
        let cases = [
            (
                json!({"kind": "ask", "answer": {"selected": []}}),
                "requestId: Invalid input: expected string, received undefined",
            ),
            (
                json!({"requestId": "r", "kind": "nope", "answer": {}}),
                "kind: Invalid discriminator value. Expected 'quiz' | 'ask' | 'plan' | 'curriculum'",
            ),
            (
                json!({"requestId": "r", "kind": "quiz", "answer": {"selected": [], "dontKnow": false, "confidence": 101}}),
                "answer.confidence: Too big: expected number to be <=100",
            ),
            (
                json!({"requestId": "r", "kind": "quiz", "answer": {"selected": [], "dontKnow": false, "note": "x".repeat(5001)}}),
                "answer.note: Too big: expected string to have <=5000 characters",
            ),
        ];
        for (body, message) in cases {
            let error = validate_answer(&body).unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST);
            assert_eq!(error.message, message);
        }
    }

    fn saved_state() -> SessionState {
        SessionState {
            id: "s".to_string(),
            config: Map::new(),
            title: "T".to_string(),
            created_at: "C".to_string(),
            updated_at: "U".to_string(),
            status: SessionStatus::Idle,
            lesson_path: None,
            usage: Usage::default(),
            items: Vec::new(),
            pending_request_id: None,
            plan: Value::Null,
            exercises: Vec::new(),
            retryable: false,
            last_error: None,
        }
    }

    #[test]
    fn state_file_keeps_node_key_order() {
        let state = saved_state();
        let file = StateFile {
            state: &state,
            provider: "claude",
            resume: Some("tok"),
            pending_start: None,
            last_turn: None,
            can_retry: false,
        };
        let json = serde_json::to_string(&file).unwrap();
        assert!(
            json.ends_with(r#""provider":"claude","resume":"tok","canRetry":false}"#),
            "{json}"
        );

        let last = LastTurn {
            text: Some("x".to_string()),
            show: vec!["x".to_string()],
            attachments: Some(json!([])),
            turn_text: "x".to_string(),
            pushed_learner: true,
        };
        let file = StateFile {
            state: &state,
            provider: "claude",
            resume: None,
            pending_start: Some("Piano · Demo"),
            last_turn: Some(&last),
            can_retry: true,
        };
        let json = serde_json::to_string(&file).unwrap();
        assert!(
            json.ends_with(r#""provider":"claude","pendingStart":"Piano · Demo","lastTurn":{"text":"x","show":["x"],"attachments":[],"turnText":"x","pushedLearner":true},"canRetry":true}"#),
            "{json}"
        );
    }

    /// A live engine handle is all `request` checks: this one never reads or writes.
    struct FakeHandle;

    impl EngineHandle for FakeHandle {
        fn send(&mut self, _text: &str) {}

        fn interrupt(&mut self) {}

        fn stop(&mut self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
            Box::pin(async {})
        }

        fn pid(&self) -> Option<u32> {
            None
        }
    }

    fn test_shared() -> (SessionManager, Arc<Shared>) {
        let shared = Arc::new(Shared {
            opts: ManagerOptions {
                repo_root: PathBuf::from("repo"),
                data_root: PathBuf::from("data"),
                code_root: PathBuf::from("code"),
                port: 4599,
                token: "test-token".to_string(),
                claude: ClaudeBin {
                    cmd: "node".to_string(),
                    args: Vec::new(),
                },
                agy: ClaudeBin {
                    cmd: "node".to_string(),
                    args: Vec::new(),
                },
                opencode: ClaudeBin {
                    cmd: "node".to_string(),
                    args: Vec::new(),
                },
                add_dirs: Vec::new(),
                events: EventBus::new(),
                limits: Arc::new(crate::limits::LimitsCache::new()),
                anki: AnkiClient::new("http://127.0.0.1:1").expect("anki client"),
                imports: ImportRegistry::new(),
                jobs: Arc::new(tools::NoJobs),
                npm_bin: None,
                installs: Default::default(),
                semantic: Arc::new(crate::semantic::SemanticSearch::for_repo(
                    Path::new("repo"),
                    crate::embeddings::EmbeddingConfig::load(Path::new("repo"))
                        .expect("embedding config"),
                )),
            },
            sessions: Mutex::new(IndexMap::new()),
            sessions_touch_pending: AtomicBool::new(false),
        });
        (SessionManager(shared.clone()), shared)
    }

    /// AC4's other half: a dropped `/mcp` call cancels its own cards and nothing else.
    #[tokio::test]
    async fn cancel_requests_only_touches_the_ids_it_is_given() {
        let (manager, shared) = test_shared();
        let session = make_session(
            &shared,
            saved_state(),
            "claude".to_string(),
            None,
            None,
            false,
        );
        session
            .lock()
            .expect("session lock")
            .turns
            .attach(Box::new(FakeHandle))
            .ok()
            .expect("a fresh handle attaches");
        shared
            .sessions
            .lock()
            .expect("sessions lock")
            .insert("s".to_string(), session.clone());

        let call: CallRequests = Default::default();
        let host = manager
            .tool_host("s", call.clone())
            .expect("the session exists");
        let receiver = host
            .request(json!({"kind": "ask", "input": {"question": "Quale?"}}))
            .expect("a card opens");
        let card = {
            let guard = session.lock().expect("session lock");
            let card = guard.data.pending.keys().next().cloned().expect("one pending");
            assert_eq!(guard.data.state.pending_request_id.as_deref(), Some(card.as_str()));
            assert_eq!(guard.data.state.status, SessionStatus::Waiting);
            card
        };

        // Ids this call never created: the card stays pending and untouched.
        manager.cancel_requests("s", &["other".to_string()]);
        {
            let guard = session.lock().expect("session lock");
            assert!(guard.data.pending.contains_key(&card));
            assert_eq!(guard.data.state.pending_request_id.as_deref(), Some(card.as_str()));
            assert_eq!(guard.data.state.status, SessionStatus::Waiting);
            assert!(
                guard
                    .data
                    .state
                    .items
                    .iter()
                    .all(|item| item.get("cancelled").is_none()),
                "{:?}",
                guard.data.state.items
            );
        }

        // Its own ids: the card is cancelled, the call unblocks with `Cancelled`.
        let ids = call.lock().expect("call lock").clone();
        assert_eq!(ids, [card.clone()]);
        manager.cancel_requests("s", &ids);
        {
            let guard = session.lock().expect("session lock");
            assert!(guard.data.pending.is_empty());
            assert_eq!(guard.data.state.pending_request_id, None);
            assert_eq!(guard.data.state.status, SessionStatus::Running);
            assert_eq!(guard.data.state.items[0]["cancelled"], json!(true));
        }
        assert!(
            matches!(receiver.await, Ok(Err(Cancelled))),
            "the call must wake with Cancelled"
        );
    }

    #[tokio::test]
    async fn concurrent_upsert_exercise_keeps_both_entries() {
        let (manager, shared) = test_shared();
        let session = make_session(
            &shared,
            saved_state(),
            "claude".to_string(),
            None,
            None,
            false,
        );
        shared
            .sessions
            .lock()
            .expect("sessions lock")
            .insert("s".to_string(), session.clone());

        let host = Arc::new(
            manager
                .tool_host("s", Default::default())
                .expect("the session exists"),
        );
        let h1 = host.clone();
        let h2 = host.clone();
        let t1 = tokio::spawn(async move {
            h1.upsert_exercise(json!({"name": "01-first"}));
        });
        let t2 = tokio::spawn(async move {
            h2.upsert_exercise(json!({"name": "02-second"}));
        });
        t1.await.unwrap();
        t2.await.unwrap();

        let list = host.exercises();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["name"], json!("01-first"));
        assert_eq!(list[1]["name"], json!("02-second"));

        let guard = session.lock().expect("session lock");
        assert_eq!(guard.data.state.exercises, list);
        let item_ids: Vec<_> = guard
            .data
            .state
            .items
            .iter()
            .filter_map(|it| it.get("id").and_then(Value::as_str))
            .collect();
        assert!(item_ids.contains(&"exercise:01-first"));
        assert!(item_ids.contains(&"exercise:02-second"));
    }
}
