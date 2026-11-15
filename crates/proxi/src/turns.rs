//! The engine-agnostic turn state machine (port of `app/server/turns.ts`).
//!
//! Synchronous by design: the one await (starting the engine) lives outside, between `begin_send` and
//! `attach`/`finish_send`. A generation number stands in for Node's `mine()` closure.

use std::collections::VecDeque;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::{CLAUDE_AUTH_ERROR_MESSAGE, EngineEvent, EngineHandle, is_auth_error};
use crate::protocol::SessionStatus;

/// Node's WATCHDOG_MS: an interrupted turn that never reports its result is given up.
pub const WATCHDOG: Duration = Duration::from_secs(10);

/// The last turn sent: re-sent on a fresh conversation when the engine refuses the resume token.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LastTurn {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub show: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Value>,
    pub turn_text: String,
    #[serde(default)]
    pub pushed_learner: bool,
}

/// What the turn module needs from its session.
pub trait TurnHost {
    fn status(&self) -> SessionStatus;
    /// `retryable` is the session's `Turns::can_retry()` at the call (Node reads it inside `setStatus`).
    fn set_status(&mut self, status: SessionStatus, error: Option<String>, retryable: bool);
    fn notice(&mut self, level: &str, text: &str);
    /// A learner bubble: pushed only once its turn really goes out. Attachments ride on the last
    /// bubble of the turn; the host adds them only when it is a non-empty array.
    fn learner(&mut self, text: &str, attachments: Option<&Value>);
    /// Engine events to render. `late` = result of a turn already given up.
    fn event(&mut self, e: &EngineEvent, late: bool);
    /// Interactive cards blocking the teacher; `cancel_pending` rejects them all.
    fn pending(&self) -> usize;
    fn cancel_pending(&mut self);
}

/// What the caller must do after a turn-module call, outside the session lock.
pub enum TurnAction {
    None,
    Stop(Box<dyn EngineHandle>),
    RelaunchFresh(String),
}

pub struct Turns {
    /// The persisted token; `None` starts a new conversation.
    pub resume: Option<String>,
    handle: Option<Box<dyn EngineHandle>>,
    /// Bumped on every attach: messages of an older process are ignored.
    generation: u64,
    stopped: bool,
    /// Turns sent / results received by the current handle: results < sends = a given-up turn's result is due.
    sends: u32,
    results: u32,
    last_sent: Option<String>,
    last_turn: Option<LastTurn>,
    can_retry_flag: bool,
    refused: bool,
    auth_failed: bool,
    auth_notice_emitted: bool,
    /// A watchdog is expected to end the interrupted turn.
    interrupting: bool,
    watchdog_seq: u64,
    /// The ticket of the watchdog that must still fire; a stale one is a no-op.
    watchdog_armed: Option<u64>,
    /// Last stderr lines of the current process.
    stderr: VecDeque<String>,
}

impl Turns {
    pub fn new(resume: Option<String>, last_turn: Option<LastTurn>, can_retry: bool) -> Self {
        Self {
            resume,
            handle: None,
            generation: 0,
            stopped: false,
            sends: 0,
            results: 0,
            last_sent: None,
            last_turn,
            can_retry_flag: can_retry,
            refused: false,
            auth_failed: false,
            auth_notice_emitted: false,
            interrupting: false,
            watchdog_seq: 0,
            watchdog_armed: None,
            stderr: VecDeque::new(),
        }
    }

    pub fn can_retry(&self) -> bool {
        self.can_retry_flag && !self.stopped && self.last_turn.is_some()
    }

    pub fn saved_last_turn(&self) -> Option<&LastTurn> {
        self.last_turn.as_ref()
    }

    pub fn live(&self) -> bool {
        self.handle.is_some()
    }

    pub fn pid(&self) -> Option<u32> {
        self.handle.as_ref().and_then(|handle| handle.pid())
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// Records the turn and, when there is no engine, asks the caller to launch one.
    pub fn begin_send<H: TurnHost + ?Sized>(
        &mut self,
        host: &mut H,
        text: Option<String>,
        show: Vec<String>,
        attachments: Option<Value>,
    ) -> bool {
        let turn_text = match attachments.as_ref() {
            Some(Value::Array(items)) if !items.is_empty() => {
                let lines = items
                    .iter()
                    .map(|item| {
                        format!(
                            "- {} (percorso: {})",
                            item["name"].as_str().unwrap_or(""),
                            item["path"].as_str().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let block = format!("[Allegati forniti dallo studente:\n{lines}]");
                match &text {
                    Some(text) => format!("{block}\n\n{text}"),
                    None => block,
                }
            }
            _ => text.clone().unwrap_or_default(),
        };
        self.last_turn = Some(LastTurn {
            text,
            show,
            attachments,
            turn_text,
            pushed_learner: false,
        });
        self.can_retry_flag = false;
        if self.handle.is_none() {
            self.set_status(host, SessionStatus::Starting, None);
            return true;
        }
        false
    }

    /// Re-arms the saved turn: `Ok(true)` asks the caller for a fresh launch, `Ok(false)` only for
    /// an attach-less re-send. The caller then runs `finish_send`.
    pub fn begin_retry<H: TurnHost + ?Sized>(&mut self, host: &mut H) -> Result<bool, &'static str> {
        if self.stopped {
            return Err("Session is closed");
        }
        if !self.can_retry() {
            return Err("Nessun turno fallito da riprovare");
        }
        self.can_retry_flag = false;
        if self.handle.is_none() {
            self.set_status(host, SessionStatus::Starting, None);
            return Ok(true);
        }
        Ok(false)
    }

    /// The launch raised: the turn stays saved (retryable) and the session shows the error.
    pub fn launch_failed<H: TurnHost + ?Sized>(&mut self, host: &mut H, message: &str) {
        self.can_retry_flag = true;
        if !self.stopped {
            self.set_status(host, SessionStatus::Error, Some(message.to_string()));
        }
    }

    /// Stores a freshly started engine. The caller stops the returned handle when the session closed meanwhile.
    pub fn attach(&mut self, handle: Box<dyn EngineHandle>) -> Result<u64, Box<dyn EngineHandle>> {
        if self.stopped {
            return Err(handle);
        }
        self.handle = Some(handle);
        self.generation += 1;
        self.sends = 0;
        self.results = 0;
        self.refused = false;
        self.auth_failed = false;
        self.auth_notice_emitted = false;
        self.stderr.clear();
        Ok(self.generation)
    }

    /// Pushes the learner bubbles, sends the turn and marks the session running.
    pub fn finish_send<H: TurnHost + ?Sized>(&mut self, host: &mut H) {
        let text = match self.last_turn.as_mut() {
            Some(turn) => {
                if !turn.pushed_learner {
                    for (index, line) in turn.show.iter().enumerate() {
                        let attachments = if index + 1 == turn.show.len() {
                            turn.attachments.as_ref()
                        } else {
                            None
                        };
                        host.learner(line, attachments);
                    }
                    turn.pushed_learner = true;
                }
                Some(turn.turn_text.clone())
            }
            None => None,
        };
        match text {
            Some(text) => {
                self.write(&text);
                self.set_status(host, SessionStatus::Running, None);
            }
            // Interrupted while starting: the turn never went out, so there is nothing to wait for.
            None => self.set_status(host, SessionStatus::Idle, None),
        }
    }

    /// Aborts the running turn; `Some(ticket)` asks the caller to arm the watchdog with it.
    pub fn interrupt<H: TurnHost + ?Sized>(&mut self, host: &mut H) -> Option<u64> {
        let busy = matches!(host.status(), SessionStatus::Running | SessionStatus::Waiting)
            && self.handle.is_some();
        if busy {
            self.interrupting = true;
        }
        self.can_retry_flag = false;
        self.last_turn = None;
        if let Some(handle) = self.handle.as_mut() {
            handle.interrupt();
        }
        host.cancel_pending();
        if !busy {
            return None;
        }
        self.watchdog_seq += 1;
        self.watchdog_armed = Some(self.watchdog_seq);
        Some(self.watchdog_seq)
    }

    /// The interrupt watchdog: gives up a turn that never reported its result.
    pub fn watchdog_fired<H: TurnHost + ?Sized>(&mut self, host: &mut H, ticket: u64) {
        if self.watchdog_armed != Some(ticket) {
            return;
        }
        self.watchdog_armed = None;
        if !self.interrupting {
            return;
        }
        self.interrupting = false;
        host.notice("info", "Turno interrotto.");
        if host.status().is_busy() {
            self.set_status(host, SessionStatus::Idle, None);
        }
    }

    /// Re-sends the same text on a fresh engine after a refused resume.
    pub fn resend<H: TurnHost + ?Sized>(&mut self, host: &mut H, text: &str) {
        self.write(text);
        self.set_status(host, SessionStatus::Running, None);
    }

    /// The fresh relaunch raised (Node's `relaunchFresh` catch): the error stays visible, nothing is retried.
    pub fn relaunch_failed<H: TurnHost + ?Sized>(&mut self, host: &mut H, message: &str) {
        if !self.stopped {
            self.set_status(host, SessionStatus::Error, Some(message.to_string()));
        }
    }

    pub fn on_stderr(&mut self, generation: u64, line: &str) {
        if !self.mine(generation) {
            return;
        }
        self.stderr.push_back(line.to_string());
        if self.stderr.len() > 20 {
            self.stderr.pop_front();
        }
        if is_auth_error(line) {
            self.auth_failed = true;
        }
    }

    pub fn on_event<H: TurnHost + ?Sized>(
        &mut self,
        host: &mut H,
        generation: u64,
        event: &EngineEvent,
    ) -> TurnAction {
        if !self.mine(generation) {
            return TurnAction::None;
        }
        match event {
            EngineEvent::AuthFailed { message } => {
                self.auth_failed = true;
                let message = message
                    .clone()
                    .unwrap_or_else(|| CLAUDE_AUTH_ERROR_MESSAGE.to_string());
                if !self.auth_notice_emitted {
                    self.auth_notice_emitted = true;
                    host.notice("error", &message);
                }
                self.set_status(host, SessionStatus::Error, Some(message));
                return match self.stop() {
                    Some(handle) => TurnAction::Stop(handle),
                    None => TurnAction::None,
                };
            }
            EngineEvent::ResumeFailed => {
                self.refused = true;
                // The next launch is fresh, so this cannot loop.
                self.resume = None;
            }
            EngineEvent::ActionDenied { action } => {
                host.notice("info", &format!("Il motore ha bloccato un'azione ({action})."));
            }
            EngineEvent::Init { session_id, .. } => {
                if !session_id.is_empty() {
                    self.resume = Some(session_id.clone());
                }
                host.event(event, false);
            }
            EngineEvent::Result {
                is_error,
                subtype,
                text,
                session_id,
                ..
            } => {
                if !session_id.is_empty() {
                    self.resume = Some(session_id.clone());
                }
                self.results += 1;
                let late = self.results < self.sends;
                host.event(event, late);
                if late {
                    return TurnAction::None;
                }
                if self.interrupting {
                    host.notice("info", "Turno interrotto.");
                    self.can_retry_flag = false;
                    self.last_turn = None;
                } else if *is_error && !self.auth_failed {
                    let mut error = format!("Errore del docente ({subtype})");
                    if let Some(text) = text.as_deref().filter(|text| !text.is_empty()) {
                        error.push_str(": ");
                        error.push_str(text);
                    }
                    host.notice("error", &error);
                    self.can_retry_flag = true;
                    if host.pending() == 0 {
                        self.set_status(host, SessionStatus::Error, Some(error));
                    }
                    self.interrupting = false;
                    self.watchdog_armed = None;
                    return TurnAction::None;
                } else {
                    self.can_retry_flag = false;
                    self.last_turn = None;
                }
                self.interrupting = false;
                self.watchdog_armed = None;
                if host.pending() == 0 && !self.auth_failed {
                    self.set_status(host, SessionStatus::Idle, None);
                }
            }
            _ => host.event(event, false),
        }
        TurnAction::None
    }

    pub fn on_exit<H: TurnHost + ?Sized>(
        &mut self,
        host: &mut H,
        generation: u64,
        code: Option<i32>,
        signal: Option<String>,
    ) -> TurnAction {
        if !self.mine(generation) {
            return TurnAction::None;
        }
        self.handle = None;
        self.interrupting = false;
        self.watchdog_armed = None;
        host.cancel_pending();
        let stderr = self.stderr.iter().cloned().collect::<Vec<_>>().join(" ");
        if self.auth_failed || is_auth_error(&stderr) {
            self.auth_failed = true;
            if !self.auth_notice_emitted {
                self.auth_notice_emitted = true;
                host.notice("error", CLAUDE_AUTH_ERROR_MESSAGE);
            }
            self.set_status(
                host,
                SessionStatus::Error,
                Some(CLAUDE_AUTH_ERROR_MESSAGE.to_string()),
            );
            return TurnAction::None;
        }
        if self.refused && self.last_sent.is_some() && !self.stopped {
            host.notice(
                "info",
                "Ripresa della conversazione non riuscita: riparto da una sessione nuova con il contesto della lezione.",
            );
            self.set_status(host, SessionStatus::Starting, None);
            return TurnAction::RelaunchFresh(self.last_sent.clone().unwrap_or_default());
        }
        if self.sends > self.results && !self.stopped {
            self.can_retry_flag = true;
        }
        let tail = self
            .stderr
            .iter()
            .rev()
            .take(5)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        // Node prints `signal ?? code`, i.e. "null" when neither is known.
        let reason = signal
            .or_else(|| code.map(|code| code.to_string()))
            .unwrap_or_else(|| "null".to_string());
        let message = if tail.is_empty() {
            format!("teacher process exited ({reason})")
        } else {
            format!("teacher process exited ({reason}): {tail}")
        };
        self.set_status(host, SessionStatus::Error, Some(message));
        TurnAction::None
    }

    /// Close/shutdown: no launch ever again; the caller awaits the returned handle's `stop()`.
    pub fn stop(&mut self) -> Option<Box<dyn EngineHandle>> {
        self.stopped = true;
        self.watchdog_armed = None;
        self.handle.take()
    }

    fn set_status<H: TurnHost + ?Sized>(
        &self,
        host: &mut H,
        status: SessionStatus,
        error: Option<String>,
    ) {
        host.set_status(status, error, self.can_retry());
    }

    fn write(&mut self, text: &str) {
        self.last_sent = Some(text.to_string());
        self.sends += 1;
        if let Some(handle) = self.handle.as_mut() {
            handle.send(text);
        }
    }

    fn mine(&self, generation: u64) -> bool {
        self.handle.is_some() && generation == self.generation
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;

    #[derive(Default)]
    struct EngineLog {
        sent: Vec<String>,
        stops: usize,
        interrupts: usize,
    }

    struct MemoryHandle {
        log: Arc<Mutex<EngineLog>>,
        pid: Option<u32>,
    }

    impl EngineHandle for MemoryHandle {
        fn send(&mut self, text: &str) {
            self.log.lock().unwrap().sent.push(text.to_string());
        }

        fn interrupt(&mut self) {
            self.log.lock().unwrap().interrupts += 1;
        }

        fn stop(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
            let log = self.log.clone();
            Box::pin(async move {
                log.lock().unwrap().stops += 1;
            })
        }

        fn pid(&self) -> Option<u32> {
            self.pid
        }
    }

    struct Host {
        status: SessionStatus,
        errors: Vec<String>,
        notices: Vec<(String, String)>,
        learner: Vec<(String, Option<Value>)>,
        late: Vec<bool>,
        events: usize,
        pending: usize,
        retryable: Vec<bool>,
    }

    impl Host {
        fn new() -> Self {
            Self {
                status: SessionStatus::Idle,
                errors: Vec::new(),
                notices: Vec::new(),
                learner: Vec::new(),
                late: Vec::new(),
                events: 0,
                pending: 0,
                retryable: Vec::new(),
            }
        }
    }

    impl TurnHost for Host {
        fn status(&self) -> SessionStatus {
            self.status
        }

        fn set_status(&mut self, status: SessionStatus, error: Option<String>, retryable: bool) {
            self.status = status;
            self.retryable.push(retryable);
            if let Some(error) = error {
                self.errors.push(error);
            }
        }

        fn notice(&mut self, level: &str, text: &str) {
            self.notices.push((level.to_string(), text.to_string()));
        }

        fn learner(&mut self, text: &str, attachments: Option<&Value>) {
            self.learner.push((text.to_string(), attachments.cloned()));
        }

        fn event(&mut self, event: &EngineEvent, late: bool) {
            self.events += 1;
            if matches!(event, EngineEvent::Result { .. }) {
                self.late.push(late);
            }
        }

        fn pending(&self) -> usize {
            self.pending
        }

        fn cancel_pending(&mut self) {
            self.pending = 0;
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime")
            .block_on(future)
    }

    fn harness(resume: Option<&str>) -> (Turns, Host, Arc<Mutex<EngineLog>>) {
        (
            Turns::new(resume.map(str::to_string), None, false),
            Host::new(),
            Arc::new(Mutex::new(EngineLog::default())),
        )
    }

    fn learner_texts(host: &Host) -> Vec<&str> {
        host.learner.iter().map(|line| line.0.as_str()).collect()
    }

    fn handle(log: &Arc<Mutex<EngineLog>>, pid: u32) -> Box<dyn EngineHandle> {
        Box::new(MemoryHandle {
            log: log.clone(),
            pid: Some(pid),
        })
    }

    fn launch(turns: &mut Turns, log: &Arc<Mutex<EngineLog>>) -> u64 {
        turns
            .attach(handle(log, 12345))
            .ok()
            .expect("the session is open")
    }

    /// begin_send + attach + finish_send: the usual one-shot send of the tests.
    fn send(turns: &mut Turns, host: &mut Host, log: &Arc<Mutex<EngineLog>>, text: &str) -> u64 {
        assert!(turns.begin_send(host, Some(text.to_string()), vec![text.to_string()], None));
        let generation = launch(turns, log);
        turns.finish_send(host);
        generation
    }

    fn init(token: &str) -> EngineEvent {
        EngineEvent::Init {
            session_id: token.to_string(),
            tools: Vec::new(),
            mcp_servers: Vec::new(),
        }
    }

    fn result_event() -> EngineEvent {
        EngineEvent::Result {
            is_error: false,
            subtype: "success".to_string(),
            text: None,
            session_id: String::new(),
            input_tokens: 0.0,
            output_tokens: 0.0,
            cache_read_tokens: 0.0,
            cache_creation_tokens: 0.0,
            cost_usd: 0.0,
        }
    }

    fn error_result(subtype: &str, text: Option<&str>) -> EngineEvent {
        EngineEvent::Result {
            is_error: true,
            subtype: subtype.to_string(),
            text: text.map(str::to_string),
            session_id: String::new(),
            input_tokens: 0.0,
            output_tokens: 0.0,
            cache_read_tokens: 0.0,
            cache_creation_tokens: 0.0,
            cost_usd: 0.0,
        }
    }

    #[test]
    fn first_send_starts_fresh_and_the_result_idles() {
        let (mut turns, mut host, log) = harness(None);

        assert!(turns.begin_send(
            &mut host,
            Some("ciao".to_string()),
            vec!["ciao".to_string()],
            None
        ));
        assert_eq!(host.status, SessionStatus::Starting);
        assert_eq!(host.retryable.last(), Some(&false));
        let generation = launch(&mut turns, &log);
        turns.finish_send(&mut host);

        assert_eq!(host.status, SessionStatus::Running);
        assert_eq!(learner_texts(&host), ["ciao"]);
        assert_eq!(log.lock().unwrap().sent, ["ciao"]);
        assert_eq!(turns.resume, None);

        assert!(matches!(
            turns.on_event(&mut host, generation, &init("tok-1")),
            TurnAction::None
        ));
        assert_eq!(turns.resume.as_deref(), Some("tok-1"));
        turns.on_event(&mut host, generation, &result_event());
        assert_eq!(host.status, SessionStatus::Idle);
        assert_eq!(turns.resume.as_deref(), Some("tok-1"));
    }

    #[test]
    fn refused_resume_asks_for_a_fresh_relaunch() {
        let (mut turns, mut host, log) = harness(Some("old-token"));
        let generation = send(&mut turns, &mut host, &log, "dopo il crash");
        turns.on_event(&mut host, generation, &EngineEvent::ResumeFailed);
        assert_eq!(turns.resume, None);

        match turns.on_exit(&mut host, generation, Some(1), None) {
            TurnAction::RelaunchFresh(text) => assert_eq!(text, "dopo il crash"),
            _ => panic!("the refused resume must relaunch fresh"),
        }
        assert_eq!(host.status, SessionStatus::Starting);
        assert_eq!(host.notices[0].0, "info");
        assert!(
            host.notices[0]
                .1
                .contains("Ripresa della conversazione non riuscita")
        );

        // The caller relaunches and re-sends: attach resets the refusal, resend writes once.
        let generation = launch(&mut turns, &log);
        turns.resend(&mut host, "dopo il crash");
        assert_eq!(host.status, SessionStatus::Running);
        assert_eq!(learner_texts(&host), ["dopo il crash"]);
        assert_eq!(log.lock().unwrap().sent, ["dopo il crash", "dopo il crash"]);
        turns.on_event(&mut host, generation, &init("new-token"));
        assert_eq!(turns.resume.as_deref(), Some("new-token"));
    }

    #[test]
    fn an_exit_without_a_refused_resume_is_an_error_and_the_next_send_keeps_the_token() {
        let (mut turns, mut host, log) = harness(None);
        let generation = send(&mut turns, &mut host, &log, "x");
        turns.on_event(&mut host, generation, &init("tok"));
        host.pending = 2;
        turns.on_stderr(generation, "boom");

        assert!(matches!(
            turns.on_exit(&mut host, generation, Some(1), None),
            TurnAction::None
        ));
        assert_eq!(host.pending, 0);
        assert_eq!(host.status, SessionStatus::Error);
        assert!(
            host.errors[0].contains("exited (1): boom"),
            "{:?}",
            host.errors
        );
        assert_eq!(host.retryable.last(), Some(&true));
        assert!(!turns.live());

        assert!(turns.begin_send(
            &mut host,
            Some("y".to_string()),
            vec!["y".to_string()],
            None
        ));
        assert_eq!(turns.resume.as_deref(), Some("tok"));
    }

    #[test]
    fn stop_takes_the_handle_and_refuses_to_launch_again() {
        let (mut turns, mut host, log) = harness(None);
        let generation = send(&mut turns, &mut host, &log, "x");

        let mut live = turns.stop().expect("the live handle");
        block_on(live.stop());
        assert_eq!(log.lock().unwrap().stops, 1);
        assert!(turns.is_stopped());

        assert!(matches!(
            turns.on_exit(&mut host, generation, None, None),
            TurnAction::None
        ));
        assert_eq!(
            host.status,
            SessionStatus::Running,
            "an exit after stop() is not an error"
        );

        assert!(
            turns.attach(handle(&log, 2)).is_err(),
            "a stopped session never launches again"
        );
        assert!(turns.begin_send(
            &mut host,
            Some("z".to_string()),
            vec!["z".to_string()],
            None
        ));
        assert_eq!(log.lock().unwrap().sent, ["x"], "nothing goes out");
    }

    #[test]
    fn a_model_error_reports_the_teacher_error_and_enables_retry() {
        let (mut turns, mut host, log) = harness(None);
        let generation = send(&mut turns, &mut host, &log, "Qual è la derivata di x^2?");
        assert!(!turns.can_retry());
        assert_eq!(learner_texts(&host), ["Qual è la derivata di x^2?"]);

        turns.on_event(&mut host, generation, &init("tok-err"));
        turns.on_event(
            &mut host,
            generation,
            &error_result("error_during_execution", Some("Rate limit reached")),
        );

        assert_eq!(host.status, SessionStatus::Error);
        assert!(turns.can_retry());
        assert_eq!(host.retryable.last(), Some(&true));
        assert!(
            host.notices.iter().any(|(level, text)| level == "error"
                && text.contains("Errore del docente")
                && text.contains("Rate limit reached")),
            "{:?}",
            host.notices
        );

        // A new send clears the flag; a clean result idles and forgets the turn.
        assert!(!turns.begin_send(
            &mut host,
            Some("di nuovo".to_string()),
            vec!["di nuovo".to_string()],
            None
        ));
        assert!(!turns.can_retry());
        turns.finish_send(&mut host);
        turns.on_event(&mut host, generation, &result_event());
        assert_eq!(host.status, SessionStatus::Idle);
        assert!(!turns.can_retry());
        assert!(turns.saved_last_turn().is_none());
    }

    #[test]
    fn a_crash_enables_retry_and_the_next_send_keeps_the_resume_token() {
        let (mut turns, mut host, log) = harness(None);
        let generation = send(&mut turns, &mut host, &log, "Spiegazione");
        turns.on_event(&mut host, generation, &init("tok-crash"));

        turns.on_stderr(generation, "SIGSEGV");
        turns.on_exit(&mut host, generation, Some(139), None);

        assert_eq!(host.status, SessionStatus::Error);
        assert!(turns.can_retry());
        assert!(!turns.live());
        assert!(
            host.errors[0].contains("teacher process exited (139): SIGSEGV"),
            "{:?}",
            host.errors
        );

        assert!(turns.begin_send(
            &mut host,
            Some("Spiegazione".to_string()),
            vec!["Spiegazione".to_string()],
            None
        ));
        assert_eq!(turns.resume.as_deref(), Some("tok-crash"));
    }

    #[test]
    fn a_failed_launch_is_retryable_and_reports_the_message() {
        let (mut turns, mut host, _log) = harness(None);
        assert!(turns.begin_send(
            &mut host,
            Some("x".to_string()),
            vec!["x".to_string()],
            None
        ));

        turns.launch_failed(&mut host, "no binary");

        assert_eq!(host.status, SessionStatus::Error);
        assert_eq!(host.errors, ["no binary"]);
        assert!(turns.can_retry());
        assert_eq!(host.retryable.last(), Some(&true));
    }

    #[test]
    fn a_failed_relaunch_shows_the_error_and_keeps_the_turn() {
        let (mut turns, mut host, log) = harness(Some("old-token"));
        let generation = send(&mut turns, &mut host, &log, "dopo il crash");
        turns.on_event(&mut host, generation, &EngineEvent::ResumeFailed);
        assert!(matches!(
            turns.on_exit(&mut host, generation, Some(1), None),
            TurnAction::RelaunchFresh(_)
        ));

        turns.relaunch_failed(&mut host, "spawn boom");

        assert_eq!(host.status, SessionStatus::Error);
        assert_eq!(host.errors.last().map(String::as_str), Some("spawn boom"));
        assert!(turns.saved_last_turn().is_some());
    }

    #[test]
    fn auth_failed_event_guides_once_and_stops_the_engine() {
        let (mut turns, mut host, log) = harness(None);
        let generation = send(&mut turns, &mut host, &log, "ciao");

        match turns.on_event(
            &mut host,
            generation,
            &EngineEvent::AuthFailed { message: None },
        ) {
            TurnAction::Stop(mut handle) => block_on(handle.stop()),
            _ => panic!("the failed process must be stopped"),
        }

        assert_eq!(host.status, SessionStatus::Error);
        assert_eq!(
            host.errors.last().map(String::as_str),
            Some(CLAUDE_AUTH_ERROR_MESSAGE)
        );
        assert!(
            host.notices
                .iter()
                .any(|(level, text)| level == "error" && text == CLAUDE_AUTH_ERROR_MESSAGE)
        );
        assert!(turns.is_stopped());
        assert_eq!(log.lock().unwrap().stops, 1);

        // The later result of the failed turn must not reset the status.
        assert!(matches!(
            turns.on_event(&mut host, generation, &error_result("api_error", None)),
            TurnAction::None
        ));
        assert_eq!(host.status, SessionStatus::Error);
    }

    #[test]
    fn an_exit_after_an_auth_error_in_stderr_guides_without_the_exit_message() {
        let (mut turns, mut host, log) = harness(None);
        let generation = send(&mut turns, &mut host, &log, "ciao");

        turns.on_stderr(generation, "Not logged in · Please run /login");
        turns.on_exit(&mut host, generation, Some(1), None);

        assert_eq!(host.status, SessionStatus::Error);
        assert_eq!(
            host.errors.last().map(String::as_str),
            Some(CLAUDE_AUTH_ERROR_MESSAGE)
        );
        assert!(
            host.notices
                .iter()
                .any(|(level, text)| level == "error" && text == CLAUDE_AUTH_ERROR_MESSAGE)
        );
        assert!(
            !host
                .errors
                .last()
                .unwrap()
                .contains("teacher process exited (1)")
        );
    }

    #[test]
    fn a_late_result_of_an_older_send_does_not_idle_the_newer_turn() {
        let (mut turns, mut host, log) = harness(None);
        let generation = send(&mut turns, &mut host, &log, "uno");

        assert!(!turns.begin_send(
            &mut host,
            Some("due".to_string()),
            vec!["due".to_string()],
            None
        ));
        turns.finish_send(&mut host);

        turns.on_event(&mut host, generation, &result_event());
        assert_eq!(host.status, SessionStatus::Running);
        turns.on_event(&mut host, generation, &result_event());
        assert_eq!(host.status, SessionStatus::Idle);
        assert_eq!(host.late, [true, false]);
    }

    #[test]
    fn action_denied_is_an_info_notice_and_not_forwarded() {
        let (mut turns, mut host, log) = harness(None);
        let generation = send(&mut turns, &mut host, &log, "ciao");
        host.events = 0;
        let before = host.status;

        assert!(matches!(
            turns.on_event(
                &mut host,
                generation,
                &EngineEvent::ActionDenied {
                    action: "ReadUrlContent".to_string()
                }
            ),
            TurnAction::None
        ));

        assert_eq!(
            host.notices.last(),
            Some(&(
                "info".to_string(),
                "Il motore ha bloccato un'azione (ReadUrlContent).".to_string()
            ))
        );
        assert_eq!(host.events, 0, "the card is a notice, not an event");
        assert_eq!(host.status, before);
    }

    #[test]
    fn messages_of_a_stale_generation_are_ignored() {
        let (mut turns, mut host, log) = harness(None);
        let generation = send(&mut turns, &mut host, &log, "x");
        let stale = generation - 1;

        turns.on_stderr(stale, "Not logged in · Please run /login");
        assert!(matches!(
            turns.on_event(&mut host, stale, &init("other")),
            TurnAction::None
        ));
        assert_eq!(turns.resume, None);
        assert!(matches!(
            turns.on_exit(&mut host, stale, Some(1), None),
            TurnAction::None
        ));
        assert_eq!(host.status, SessionStatus::Running);
        assert!(host.errors.is_empty());

        // A replaced handle's generation is stale too.
        let current = launch(&mut turns, &log);
        assert!(matches!(
            turns.on_exit(&mut host, generation, Some(1), None),
            TurnAction::None
        ));
        assert_eq!(host.status, SessionStatus::Running);

        turns.on_event(&mut host, current, &init("tok"));
        assert_eq!(turns.resume.as_deref(), Some("tok"));
    }

    #[test]
    fn the_saved_turn_keeps_node_key_order_and_attachments_only_when_given() {
        let (mut turns, mut host, _log) = harness(None);

        assert!(turns.begin_send(
            &mut host,
            Some("x".to_string()),
            vec!["x".to_string()],
            Some(json!([]))
        ));
        assert_eq!(
            serde_json::to_string(turns.saved_last_turn().unwrap()).unwrap(),
            r#"{"text":"x","show":["x"],"attachments":[],"turnText":"x","pushedLearner":false}"#
        );

        assert!(turns.begin_send(
            &mut host,
            Some("y".to_string()),
            vec!["y".to_string()],
            None
        ));
        assert_eq!(
            serde_json::to_string(turns.saved_last_turn().unwrap()).unwrap(),
            r#"{"text":"y","show":["y"],"turnText":"y","pushedLearner":false}"#
        );
    }

    #[test]
    fn interrupt_then_the_late_result_of_the_given_up_turn_does_not_idle_the_newer_turn() {
        let (mut turns, mut host, log) = harness(None);
        let generation = send(&mut turns, &mut host, &log, "uno");

        let ticket = turns
            .interrupt(&mut host)
            .expect("a busy turn arms the watchdog");
        assert_eq!(log.lock().unwrap().interrupts, 1);
        turns.watchdog_fired(&mut host, ticket);
        assert_eq!(host.status, SessionStatus::Idle);
        assert_eq!(
            host.notices,
            [("info".to_string(), "Turno interrotto.".to_string())]
        );

        assert!(!turns.begin_send(
            &mut host,
            Some("due".to_string()),
            vec!["due".to_string()],
            None
        ));
        turns.finish_send(&mut host);
        turns.on_event(&mut host, generation, &result_event()); // late result of "uno"
        assert_eq!(host.status, SessionStatus::Running);
        turns.on_event(&mut host, generation, &result_event());
        assert_eq!(host.status, SessionStatus::Idle);
        assert_eq!(host.late, [true, false]);
    }

    #[test]
    fn interrupt_with_a_prompt_result_gives_one_notice_and_disarms_the_watchdog() {
        let (mut turns, mut host, log) = harness(None);
        let generation = send(&mut turns, &mut host, &log, "uno");

        let ticket = turns.interrupt(&mut host).expect("a busy turn");
        turns.on_event(
            &mut host,
            generation,
            &error_result("error_during_execution", None),
        );

        assert_eq!(host.status, SessionStatus::Idle);
        assert_eq!(
            host.notices,
            [("info".to_string(), "Turno interrotto.".to_string())]
        );
        turns.watchdog_fired(&mut host, ticket);
        assert_eq!(host.notices.len(), 1, "the disarmed watchdog adds nothing");
        assert!(!turns.can_retry());
        assert!(turns.saved_last_turn().is_none());
    }

    #[test]
    fn interrupt_without_a_process_only_cancels_the_cards() {
        let (mut turns, mut host, log) = harness(None);
        host.status = SessionStatus::Waiting;
        host.pending = 1;

        assert!(turns.interrupt(&mut host).is_none(), "no process to abort");
        assert_eq!(host.pending, 0);
        assert_eq!(log.lock().unwrap().interrupts, 0);
    }

    #[test]
    fn attachments_go_into_the_turn_prompt_and_onto_the_last_learner_bubble() {
        let (mut turns, mut host, log) = harness(None);
        let attachments = json!([
            {
                "id": "1",
                "name": "diagram.png",
                "path": ".learner/sessions/s/attachments/diagram.png",
                "mimeType": "image/png",
                "size": 100
            },
            {
                "id": "2",
                "name": "book.pdf",
                "path": ".learner/sessions/s/attachments/book.pdf",
                "mimeType": "application/pdf",
                "size": 500
            }
        ]);

        assert!(turns.begin_send(
            &mut host,
            Some("Spiegami questo diagramma".to_string()),
            vec![
                "apertura".to_string(),
                "Spiegami questo diagramma".to_string()
            ],
            Some(attachments.clone())
        ));
        launch(&mut turns, &log);
        turns.finish_send(&mut host);

        assert_eq!(
            log.lock().unwrap().sent,
            ["[Allegati forniti dallo studente:\n- diagram.png (percorso: .learner/sessions/s/attachments/diagram.png)\n- book.pdf (percorso: .learner/sessions/s/attachments/book.pdf)]\n\nSpiegami questo diagramma"]
        );
        assert_eq!(
            host.learner,
            [
                ("apertura".to_string(), None),
                ("Spiegami questo diagramma".to_string(), Some(attachments))
            ]
        );
        assert_eq!(
            turns.saved_last_turn().unwrap().text.as_deref(),
            Some("Spiegami questo diagramma")
        );
    }

    #[test]
    fn a_model_error_enables_retry_and_retry_resends_without_duplicating_bubbles() {
        let (mut turns, mut host, log) = harness(None);
        let generation = send(&mut turns, &mut host, &log, "Qual è la derivata di x^2?");
        turns.on_event(&mut host, generation, &init("tok-err"));
        turns.on_event(
            &mut host,
            generation,
            &error_result("error_during_execution", Some("Rate limit reached")),
        );
        assert!(turns.can_retry());

        assert_eq!(turns.begin_retry(&mut host), Ok(false));
        turns.finish_send(&mut host);
        assert_eq!(host.status, SessionStatus::Running);
        assert_eq!(learner_texts(&host), ["Qual è la derivata di x^2?"]);
        assert_eq!(
            log.lock().unwrap().sent,
            ["Qual è la derivata di x^2?", "Qual è la derivata di x^2?"]
        );

        turns.on_event(&mut host, generation, &result_event());
        assert_eq!(host.status, SessionStatus::Idle);
        assert!(!turns.can_retry());
        assert!(turns.saved_last_turn().is_none());
    }

    #[test]
    fn a_crash_enables_retry_and_retry_relaunches_with_the_resume_token() {
        let (mut turns, mut host, log) = harness(None);
        let generation = send(&mut turns, &mut host, &log, "Spiegazione");
        turns.on_event(&mut host, generation, &init("tok-crash"));
        turns.on_stderr(generation, "SIGSEGV");
        turns.on_exit(&mut host, generation, Some(139), None);
        assert_eq!(host.status, SessionStatus::Error);
        assert!(turns.can_retry());
        assert!(!turns.live());

        assert_eq!(turns.begin_retry(&mut host), Ok(true));
        assert_eq!(host.status, SessionStatus::Starting);
        assert_eq!(turns.resume.as_deref(), Some("tok-crash"));
        launch(&mut turns, &log);
        turns.finish_send(&mut host);

        assert_eq!(
            log.lock().unwrap().sent,
            ["Spiegazione", "Spiegazione"],
            "the new handle got the turn once"
        );
        assert_eq!(learner_texts(&host), ["Spiegazione"]);
        assert_eq!(host.status, SessionStatus::Running);
    }

    #[test]
    fn retry_is_refused_when_nothing_failed_or_the_session_is_closed() {
        let (mut turns, mut host, _log) = harness(None);
        assert_eq!(
            turns.begin_retry(&mut host),
            Err("Nessun turno fallito da riprovare")
        );

        let (mut turns, mut host, _log) = harness(None);
        assert!(turns.begin_send(
            &mut host,
            Some("x".to_string()),
            vec!["x".to_string()],
            None
        ));
        turns.launch_failed(&mut host, "no binary");
        assert!(turns.can_retry());
        turns.stop();
        assert_eq!(turns.begin_retry(&mut host), Err("Session is closed"));
    }

    #[test]
    fn finish_send_after_an_interrupt_during_start_idles_without_writing() {
        let (mut turns, mut host, log) = harness(None);
        assert!(turns.begin_send(
            &mut host,
            Some("x".to_string()),
            vec!["x".to_string()],
            None
        ));
        assert_eq!(host.status, SessionStatus::Starting);

        assert!(
            turns.interrupt(&mut host).is_none(),
            "starting is not a running turn"
        );
        launch(&mut turns, &log);
        turns.finish_send(&mut host);

        assert!(log.lock().unwrap().sent.is_empty(), "nothing went out");
        assert_eq!(host.status, SessionStatus::Idle);
    }
}
