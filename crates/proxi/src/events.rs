//! Server-sent event bus and ISO timestamps.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Interval of the `: keep-alive` comment on `/api/events` (same as the Node server).
pub const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// Enough headroom for a burst of `POST /api/settings` with a slow reader.
const CAPACITY: usize = 64;

/// Fan-out of pre-serialised JSON events to every open `/api/events` connection.
#[derive(Clone)]
pub struct EventBus {
    tx: tokio::sync::broadcast::Sender<String>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            tx: tokio::sync::broadcast::channel(CAPACITY).0,
        }
    }

    /// Serialise once here so all subscribers get the same bytes.
    pub fn emit<T: serde::Serialize>(&self, event: &T) {
        if let Ok(payload) = serde_json::to_string(event) {
            // Nobody listening is the common case: no events tab open.
            let _ = self.tx.send(payload);
        }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// JS `new Date(ms).toISOString()`: `YYYY-MM-DDTHH:MM:SS.mmmZ`, always 24 chars.
pub fn iso_from_millis(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = of_day / 3_600;
    let minute = of_day % 3_600 / 60;
    let second = of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

pub fn iso_now() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    iso_from_millis(ms)
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → (year, month, day).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_alive_is_fifteen_seconds() {
        assert_eq!(KEEP_ALIVE, Duration::from_secs(15));
    }

    #[test]
    fn emit_without_subscribers_does_not_panic() {
        EventBus::new().emit(&serde_json::json!({ "type": "courses" }));
    }

    #[tokio::test]
    async fn emit_reaches_every_subscriber_with_compact_json() {
        let bus = EventBus::new();
        let mut first = bus.subscribe();
        let mut second = bus.subscribe();
        let event = serde_json::json!({ "type": "courses" });

        bus.emit(&event);

        let expected = serde_json::to_string(&event).unwrap();
        assert_eq!(first.recv().await.unwrap(), expected);
        assert_eq!(second.recv().await.unwrap(), expected);
    }

    #[test]
    fn iso_matches_javascript_to_iso_string() {
        assert_eq!(iso_from_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            iso_from_millis(1_700_000_000_123),
            "2023-11-14T22:13:20.123Z"
        );
        assert_eq!(
            iso_from_millis(951_782_400_000),
            "2000-02-29T00:00:00.000Z"
        );
        assert_eq!(
            iso_from_millis(4_102_444_799_999),
            "2099-12-31T23:59:59.999Z"
        );
    }

    #[test]
    fn iso_now_has_the_to_iso_string_shape() {
        let now = iso_now();
        assert_eq!(now.len(), 24);
        assert!(now.ends_with('Z'));
    }
}
