//! Webhooks: Discord embeds and a generic JSON sink.
//!
//! Batched, because a thirty-player join wave is one thing that happened and not
//! thirty, and because Discord answers a burst with 429 rather than with
//! messages. The batching window is also what turns "the server restarted" and
//! "everybody left" into a single legible message instead of a scroll.
//!
//! Colours come from the Applejack palette rather than from Discord's defaults:
//! azure for ordinary state, orchard for good news, russet for a fault.

pub mod discord;

use std::time::Duration;

use cellar_core::config::NotifyConfig;
use cellar_core::event::Event;
use cellar_core::secret::Secret;
use tokio::sync::broadcast;

/// Sends batches to whichever sinks are configured.
pub struct Notifier {
    client: reqwest::Client,
    discord: Option<Secret>,
    generic: Option<Secret>,
    kinds: Vec<String>,
    batch: Duration,
    hostname: String,
}

impl Notifier {
    /// Build a notifier, or `None` when nothing is configured to receive.
    pub fn new(config: &NotifyConfig, hostname: impl Into<String>) -> Option<Self> {
        if !config.enabled {
            return None;
        }

        if config.discord_webhook.is_none() && config.generic_webhook.is_none() {
            return None;
        }

        let client = reqwest::Client::builder()
            // A webhook that hangs must never hold up the event loop; the
            // supervisor's job is the server, not the notification.
            .timeout(Duration::from_secs(10))
            .build()
            .ok()?;

        Some(Self {
            client,
            discord: config.discord_webhook.clone(),
            generic: config.generic_webhook.clone(),
            kinds: config.kinds.clone(),
            batch: Duration::from_secs(config.batch_seconds.max(1)),
            hostname: hostname.into(),
        })
    }

    /// Whether an event is one this notifier was asked to send.
    pub fn wants(&self, event: &Event) -> bool {
        if !event.is_notable() {
            return false;
        }

        // An empty list means every notable kind, which is the useful default:
        // an operator who has not chosen wants to be told things.
        self.kinds.is_empty() || self.kinds.iter().any(|k| k == event.kind())
    }

    /// Consume the event stream until it closes, sending batches.
    pub async fn run(self, mut events: broadcast::Receiver<Event>) {
        let mut pending: Vec<Event> = Vec::new();
        let mut ticker = tokio::time::interval(self.batch);

        loop {
            tokio::select! {
                received = events.recv() => match received {
                    Ok(event) => {
                        if self.wants(&event) {
                            pending.push(event);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!("notifier fell behind, {missed} event(s) not sent");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = ticker.tick() => {
                    if !pending.is_empty() {
                        let batch = std::mem::take(&mut pending);
                        self.send(&batch).await;
                    }
                }
            }
        }

        if !pending.is_empty() {
            self.send(&pending).await;
        }
    }

    async fn send(&self, batch: &[Event]) {
        if let Some(url) = &self.discord {
            let payload = discord::payload(batch, &self.hostname);
            self.post(url, &payload).await;
        }

        if let Some(url) = &self.generic {
            let payload = serde_json::json!({
                "hostname": self.hostname,
                "at": chrono::Utc::now().to_rfc3339(),
                "events": batch,
            });
            self.post(url, &payload).await;
        }
    }

    async fn post(&self, url: &Secret, payload: &serde_json::Value) {
        match self.client.post(url.expose()).json(payload).send().await {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => {
                // The URL is never in the message. A webhook URL is a
                // credential: anyone holding it can post as this integration.
                tracing::warn!("webhook returned {}", response.status());
            }
            Err(error) => {
                // `reqwest`'s Display includes the URL, so it is rebuilt without.
                tracing::warn!("webhook failed: {}", strip_url(&error.to_string()));
            }
        }
    }
}

/// Remove anything URL-shaped from an error before it is logged.
fn strip_url(message: &str) -> String {
    message
        .split_whitespace()
        .map(|word| if word.contains("://") { "<url>" } else { word })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use cellar_core::event::{LogLine, Origin, ResourceSample};

    use super::*;

    fn config() -> NotifyConfig {
        NotifyConfig {
            enabled: true,
            discord_webhook: Some(Secret::new("https://discord.example/webhook")),
            generic_webhook: None,
            batch_seconds: 5,
            kinds: Vec::new(),
        }
    }

    #[test]
    fn nothing_configured_means_no_notifier() {
        let mut config = config();
        config.enabled = false;
        assert!(Notifier::new(&config, "test").is_none());

        config.enabled = true;
        config.discord_webhook = None;
        assert!(Notifier::new(&config, "test").is_none());
    }

    #[test]
    fn high_frequency_events_are_never_sent() {
        let notifier = Notifier::new(&config(), "test").unwrap();

        let noisy = [
            Event::Resources(ResourceSample {
                at: chrono::Utc::now(),
                cpu_percent: 1.0,
                memory_bytes: 1,
                process_count: 1,
                host_cpu_percent: 1.0,
                host_memory_percent: 1.0,
                network_rx_bytes_per_sec: 0,
                network_tx_bytes_per_sec: 0,
            }),
            Event::Log(LogLine {
                at: chrono::Utc::now(),
                level: cellar_core::event::Level::Info,
                logger: "Identity".into(),
                message: "x".into(),
                origin: Origin::LogFile,
            }),
            Event::Unparsed {
                raw: "?".into(),
                origin: Origin::Console,
            },
        ];

        for event in noisy {
            assert!(!notifier.wants(&event), "{} must not be sent", event.kind());
        }
    }

    #[test]
    fn notable_events_are_sent_by_default() {
        let notifier = Notifier::new(&config(), "test").unwrap();

        assert!(notifier.wants(&Event::PlayerJoined {
            steam_id: 1,
            name: "Kyle".into()
        }));
        assert!(notifier.wants(&Event::ProcessExited {
            code: Some(1),
            graceful: false
        }));
    }

    #[test]
    fn an_explicit_kind_list_filters() {
        let mut config = config();
        config.kinds = vec!["process_exited".to_owned()];
        let notifier = Notifier::new(&config, "test").unwrap();

        assert!(notifier.wants(&Event::ProcessExited {
            code: Some(1),
            graceful: false
        }));
        assert!(!notifier.wants(&Event::PlayerJoined {
            steam_id: 1,
            name: "Kyle".into()
        }));
    }

    #[test]
    fn an_error_message_never_carries_the_webhook_url() {
        let message = "error sending request for url (https://discord.com/api/webhooks/123/secret)";
        let stripped = strip_url(message);
        assert!(!stripped.contains("discord.com"));
        assert!(!stripped.contains("secret"));
        assert!(stripped.contains("<url>"));
    }
}
