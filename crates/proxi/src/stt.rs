//! Voice dictation: forwards the audio to Groq or Deepgram.

use axum::body::Bytes;
use reqwest::multipart::{Form, Part};
use serde_json::Value;

use crate::protocol::LearnerSettings;

pub const GROQ_BASE: &str = "https://api.groq.com";
pub const DEEPGRAM_BASE: &str = "https://api.deepgram.com";

const TRANSCRIPTION_ERROR: &str = "Errore di trascrizione";
const FETCH_FAILED: &str = "fetch failed";

/// The recorded audio and the type the browser reported (`None` when it sent none).
pub struct Audio {
    pub bytes: Bytes,
    pub content_type: Option<String>,
}

/// Node's `sttProvider || "groq"`.
fn provider(settings: &LearnerSettings) -> &str {
    if settings.stt_provider.is_empty() {
        "groq"
    } else {
        &settings.stt_provider
    }
}

pub fn missing_key_message(provider: &str) -> &'static str {
    if provider == "groq" {
        "Nessuna API key Groq configurata. Inseriscila nelle Impostazioni (gratuita su console.groq.com)."
    } else {
        "Nessuna API key Deepgram configurata. Inseriscila nelle Impostazioni."
    }
}

/// The settings key wins; otherwise the provider's environment variable. Empty means absent.
pub fn resolve_key(
    settings: &LearnerSettings,
    env: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let key = settings.stt_api_key.trim();
    if !key.is_empty() {
        return Some(key.to_string());
    }
    let var = if provider(settings) == "groq" {
        "GROQ_API_KEY"
    } else {
        "DEEPGRAM_API_KEY"
    };
    env(var).filter(|key| !key.is_empty())
}

/// JS `encodeURIComponent`: everything but `A-Z a-z 0-9 - _ . ! ~ * ' ( )` is percent-encoded.
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            );
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The fake server replaces the service origin, like the harness `fakeFetch`.
fn origin(base: Option<&str>, default: &str) -> String {
    match base {
        Some(base) => base.trim_end_matches('/').to_string(),
        None => default.to_string(),
    }
}

fn content_type<'a>(audio: &'a Audio, default: &'a str) -> &'a str {
    audio
        .content_type
        .as_deref()
        .filter(|t| !t.is_empty())
        .unwrap_or(default)
}

/// Node sends the blob as `dictation.webm` with its own type; unknown types become octet-stream.
fn groq_form(audio: &Audio, language: &str) -> reqwest::Result<Form> {
    // `Part::bytes` keeps a `Cow<'static, _>`, so the audio has to be owned.
    let file = Part::bytes(audio.bytes.to_vec())
        .file_name("dictation.webm")
        .mime_str(content_type(audio, "application/octet-stream"))?;
    Ok(Form::new()
        .part("file", file)
        .text("model", "whisper-large-v3-turbo")
        .text("language", language.to_string())
        .text("response_format", "json")
        .text("temperature", "0"))
}

pub async fn transcribe(
    client: &reqwest::Client,
    base: Option<&str>,
    settings: &LearnerSettings,
    audio: Audio,
    env: impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let provider = provider(settings);
    let Some(key) = resolve_key(settings, env) else {
        return Err(missing_key_message(provider).to_string());
    };
    let language = settings.dictation_language.trim();
    let language = if language.is_empty() { "it" } else { language };

    if provider == "groq" {
        groq(client, origin(base, GROQ_BASE), &key, language, audio).await
    } else {
        deepgram(client, origin(base, DEEPGRAM_BASE), &key, language, audio).await
    }
}

async fn groq(
    client: &reqwest::Client,
    origin: String,
    key: &str,
    language: &str,
    audio: Audio,
) -> Result<String, String> {
    let form = groq_form(&audio, language).map_err(|_| TRANSCRIPTION_ERROR.to_string())?;
    let res = client
        .post(format!("{origin}/openai/v1/audio/transcriptions"))
        .header("Authorization", format!("Bearer {key}"))
        .multipart(form)
        .send()
        .await
        .map_err(|_| FETCH_FAILED.to_string())?;

    let status = res.status().as_u16();
    if !(200..300).contains(&status) {
        // Node's `res.text().catch(() => "")`: an unreadable error body is no body.
        let body = res.text().await.unwrap_or_default();
        let mut msg = format!("Errore Groq API ({status})");
        if let Ok(json) = serde_json::from_str::<Value>(&body) {
            if let Some(m) = json
                .pointer("/error/message")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
            {
                msg = format!("Groq: {m}");
            }
        }
        return Err(msg);
    }

    let body = res.text().await.map_err(|_| TRANSCRIPTION_ERROR.to_string())?;
    let json = serde_json::from_str::<Value>(&body).map_err(|_| TRANSCRIPTION_ERROR.to_string())?;
    let text = json.get("text").and_then(Value::as_str).unwrap_or_default();
    Ok(text.trim().to_string())
}

async fn deepgram(
    client: &reqwest::Client,
    origin: String,
    key: &str,
    language: &str,
    audio: Audio,
) -> Result<String, String> {
    let url = format!(
        "{origin}/v1/listen?model=nova-3&language={}&smart_format=true&punctuate=true",
        encode_uri_component(language)
    );
    let res = client
        .post(url)
        .header("Authorization", format!("Token {key}"))
        .header("Content-Type", content_type(&audio, "audio/webm"))
        .body(audio.bytes)
        .send()
        .await
        .map_err(|_| FETCH_FAILED.to_string())?;

    let status = res.status().as_u16();
    if !(200..300).contains(&status) {
        let body = res.text().await.unwrap_or_default();
        let mut msg = format!("Errore Deepgram API ({status})");
        if let Ok(json) = serde_json::from_str::<Value>(&body) {
            if let Some(m) = json
                .get("err_msg")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
            {
                msg = format!("Deepgram: {m}");
            }
        }
        return Err(msg);
    }

    let body = res.text().await.map_err(|_| TRANSCRIPTION_ERROR.to_string())?;
    let json = serde_json::from_str::<Value>(&body).map_err(|_| TRANSCRIPTION_ERROR.to_string())?;
    let transcript = json
        .pointer("/results/channels/0/alternatives/0/transcript")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok(transcript.trim().to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::{Request, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::Router;

    use super::*;

    #[derive(Clone)]
    struct Reply {
        status: u16,
        body: String,
    }

    impl Reply {
        fn json(status: u16, body: &str) -> Self {
            Self {
                status,
                body: body.to_string(),
            }
        }
    }

    struct Recorded {
        method: String,
        path: String,
        authorization: String,
        content_type: String,
        body: Vec<u8>,
    }

    type FakeState = (Arc<Mutex<Vec<Recorded>>>, Arc<Mutex<Reply>>);

    fn header(req: &Request, name: &str) -> String {
        req.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }

    async fn handle(State((requests, reply)): State<FakeState>, req: Request) -> Response {
        let method = req.method().to_string();
        let mut path = req.uri().path().to_string();
        if let Some(query) = req.uri().query() {
            path.push('?');
            path.push_str(query);
        }
        let authorization = header(&req, "authorization");
        let content_type = header(&req, "content-type");
        let body = axum::body::to_bytes(req.into_body(), usize::MAX)
            .await
            .unwrap_or_default()
            .to_vec();
        requests.lock().unwrap().push(Recorded {
            method,
            path,
            authorization,
            content_type,
            body,
        });
        let reply = reply.lock().unwrap().clone();
        let status = StatusCode::from_u16(reply.status).unwrap();
        (status, reply.body).into_response()
    }

    /// A fake Groq/Deepgram on a loopback port; the same reply until the test overwrites it.
    async fn fake_service(reply: Reply) -> (String, Arc<Mutex<Vec<Recorded>>>, Arc<Mutex<Reply>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let reply = Arc::new(Mutex::new(reply));
        let app = Router::new()
            .fallback(handle)
            .with_state((requests.clone(), reply.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), requests, reply)
    }

    fn fake_key() -> String {
        ["test", &"harness".repeat(3)].concat()
    }

    fn settings(provider: &str, key: &str) -> LearnerSettings {
        LearnerSettings {
            stt_provider: provider.to_string(),
            stt_api_key: key.to_string(),
            ..LearnerSettings::default()
        }
    }

    fn audio() -> Audio {
        Audio {
            bytes: Bytes::from_static(b"fake-audio"),
            content_type: Some("audio/webm".to_string()),
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[tokio::test]
    async fn missing_key_fails_per_provider() {
        let (base, requests, _reply) = fake_service(Reply::json(200, "{}")).await;
        let client = reqwest::Client::new();

        let err = transcribe(&client, Some(&base), &settings("groq", ""), audio(), no_env)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            "Nessuna API key Groq configurata. Inseriscila nelle Impostazioni (gratuita su console.groq.com)."
        );
        let err = transcribe(
            &client,
            Some(&base),
            &settings("deepgram", ""),
            audio(),
            no_env,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err,
            "Nessuna API key Deepgram configurata. Inseriscila nelle Impostazioni."
        );

        // An unset sttProvider means Groq, like Node's `|| "groq"`.
        let mut unset = LearnerSettings::default();
        unset.stt_provider.clear();
        let err = transcribe(&client, Some(&base), &unset, audio(), no_env)
            .await
            .unwrap_err();
        assert!(err.starts_with("Nessuna API key Groq"), "{err}");

        assert!(requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn groq_sends_form_and_trims_text() {
        let (base, requests, _reply) = fake_service(Reply::json(
            200,
            r#"{"text":"  Ciao, questo è un test in italiano.  "}"#,
        ))
        .await;
        let key = fake_key();
        // The base keeps a trailing slash: the origin trims it.
        let base = format!("{base}/");

        let text = transcribe(
            &reqwest::Client::new(),
            Some(&base),
            &settings("groq", &key),
            audio(),
            no_env,
        )
        .await
        .unwrap();

        assert_eq!(text, "Ciao, questo è un test in italiano.");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/openai/v1/audio/transcriptions");
        assert_eq!(req.authorization, format!("Bearer {key}"));
        assert!(
            req.content_type
                .starts_with("multipart/form-data; boundary="),
            "{}",
            req.content_type
        );

        let body = String::from_utf8_lossy(&req.body);
        for needle in [
            "name=\"model\"",
            "whisper-large-v3-turbo",
            "name=\"language\"",
            "name=\"response_format\"",
            "json",
            "name=\"temperature\"",
            "filename=\"dictation.webm\"",
            "fake-audio",
        ] {
            assert!(body.contains(needle), "missing {needle} in {body}");
        }
        assert!(body.contains("name=\"language\"\r\n\r\nit\r\n"), "{body}");
    }

    #[tokio::test]
    async fn groq_error_uses_service_message() {
        let (base, requests, reply) = fake_service(Reply::json(
            401,
            r#"{"error":{"message":"Invalid API Key"}}"#,
        ))
        .await;
        let client = reqwest::Client::new();
        let settings = settings("groq", &fake_key());

        let err = transcribe(&client, Some(&base), &settings, audio(), no_env)
            .await
            .unwrap_err();
        assert_eq!(err, "Groq: Invalid API Key");

        *reply.lock().unwrap() = Reply::json(500, "boom");
        let err = transcribe(&client, Some(&base), &settings, audio(), no_env)
            .await
            .unwrap_err();
        assert_eq!(err, "Errore Groq API (500)");
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn deepgram_sends_raw_audio() {
        let (base, requests, _reply) = fake_service(Reply::json(
            200,
            r#"{"results":{"channels":[{"alternatives":[{"transcript":"Risposta corretta. "}]}]}}"#,
        ))
        .await;
        let key = fake_key();

        let text = transcribe(
            &reqwest::Client::new(),
            Some(&base),
            &settings("deepgram", &key),
            audio(),
            no_env,
        )
        .await
        .unwrap();

        assert_eq!(text, "Risposta corretta.");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert_eq!(req.method, "POST");
        assert_eq!(
            req.path,
            "/v1/listen?model=nova-3&language=it&smart_format=true&punctuate=true"
        );
        assert_eq!(req.authorization, format!("Token {key}"));
        assert_eq!(req.content_type, "audio/webm");
        assert_eq!(req.body, b"fake-audio".to_vec());
    }

    #[tokio::test]
    async fn deepgram_error_uses_err_msg() {
        let (base, requests, reply) =
            fake_service(Reply::json(400, r#"{"err_msg":"Bad audio"}"#)).await;
        let client = reqwest::Client::new();
        let settings = settings("deepgram", &fake_key());

        let err = transcribe(&client, Some(&base), &settings, audio(), no_env)
            .await
            .unwrap_err();
        assert_eq!(err, "Deepgram: Bad audio");

        *reply.lock().unwrap() = Reply::json(503, "boom");
        let err = transcribe(&client, Some(&base), &settings, audio(), no_env)
            .await
            .unwrap_err();
        assert_eq!(err, "Errore Deepgram API (503)");
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn language_is_forwarded() {
        let (groq_base, requests, _reply) =
            fake_service(Reply::json(200, r#"{"text":"Hello world"}"#)).await;
        let mut groq_settings = settings("groq", &fake_key());
        groq_settings.dictation_language = "en".to_string();
        let text = transcribe(
            &reqwest::Client::new(),
            Some(&groq_base),
            &groq_settings,
            audio(),
            no_env,
        )
        .await
        .unwrap();
        assert_eq!(text, "Hello world");
        let body = String::from_utf8_lossy(&requests.lock().unwrap()[0].body).to_string();
        assert!(body.contains("name=\"language\"\r\n\r\nen\r\n"), "{body}");

        let (deepgram_base, requests, _reply) = fake_service(Reply::json(
            200,
            r#"{"results":{"channels":[{"alternatives":[{"transcript":"Hola mundo"}]}]}}"#,
        ))
        .await;
        let mut deepgram_settings = settings("deepgram", &fake_key());
        deepgram_settings.dictation_language = "es".to_string();
        let text = transcribe(
            &reqwest::Client::new(),
            Some(&deepgram_base),
            &deepgram_settings,
            audio(),
            no_env,
        )
        .await
        .unwrap();
        assert_eq!(text, "Hola mundo");
        assert_eq!(
            requests.lock().unwrap()[0].path,
            "/v1/listen?model=nova-3&language=es&smart_format=true&punctuate=true"
        );
    }

    #[test]
    fn key_falls_back_to_env() {
        let key = fake_key();
        let env = |name: &str| Some(format!("env-{name}"));

        assert_eq!(
            resolve_key(&settings("groq", ""), env),
            Some("env-GROQ_API_KEY".to_string())
        );
        assert_eq!(
            resolve_key(&settings("deepgram", "  "), env),
            Some("env-DEEPGRAM_API_KEY".to_string())
        );
        assert_eq!(
            resolve_key(&settings("groq", ""), |_| Some(String::new())),
            None
        );
        assert_eq!(resolve_key(&settings("groq", "  "), no_env), None);
        assert_eq!(
            resolve_key(&settings("groq", &key), |_| Some("env".to_string())),
            Some(key.clone())
        );
        assert_eq!(
            resolve_key(&settings("deepgram", &format!("  {key}  ")), no_env),
            Some(key)
        );
    }

    #[test]
    fn encode_uri_component_matches_js() {
        assert_eq!(encode_uri_component("pt-BR"), "pt-BR");
        assert_eq!(encode_uri_component("a b&c"), "a%20b%26c");
        assert_eq!(encode_uri_component("è"), "%C3%A8");
        assert_eq!(
            encode_uri_component("A-Z a_z.0~9!*'()"),
            "A-Z%20a_z.0~9!*'()"
        );
    }

    #[tokio::test]
    async fn unreachable_service_is_fetch_failed() {
        let err = transcribe(
            &reqwest::Client::new(),
            Some("http://127.0.0.1:9"),
            &settings("groq", &fake_key()),
            audio(),
            no_env,
        )
        .await
        .unwrap_err();
        assert_eq!(err, "fetch failed");
    }
}
