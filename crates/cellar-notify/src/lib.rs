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
use cellar_core::event::{Event, InstanceEvent};
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
    /// Whether this process supervises more than one server, which decides
    /// whether a message names which one it is about.
    several: bool,
}

impl Notifier {
    /// Build a notifier, or `None` when nothing is configured to receive.
    pub fn new(config: &NotifyConfig, hostname: impl Into<String>, several: bool) -> Option<Self> {
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
            several,
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

    /// Consume the merged event stream until it closes, sending batches.
    ///
    /// **The merged stream, not the primary's.** This used to subscribe to one
    /// handle, so on a two-instance deployment a crash on the second server
    /// notified nobody, which is precisely the server an unattended deployment
    /// hears about last.
    ///
    /// Batches are kept per instance rather than merged into one message. Two
    /// servers restarting for unrelated reasons in the same five second window
    /// is two things that happened, and a single embed listing both without
    /// saying which line belongs to which is worse than two messages.
    pub async fn run(self, mut events: broadcast::Receiver<InstanceEvent>) {
        let mut pending: Vec<(String, Vec<Event>)> = Vec::new();
        let mut ticker = tokio::time::interval(self.batch);

        loop {
            tokio::select! {
                received = events.recv() => match received {
                    Ok(wrapped) => {
                        if self.wants(&wrapped.event) {
                            let instance = wrapped.instance.to_string();
                            match pending.iter_mut().find(|(id, _)| *id == instance) {
                                Some((_, batch)) => batch.push(wrapped.event),
                                None => pending.push((instance, vec![wrapped.event])),
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!("notifier fell behind, {missed} event(s) not sent");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = ticker.tick() => {
                    for (instance, batch) in std::mem::take(&mut pending) {
                        self.send(&instance, &batch).await;
                    }
                }
            }
        }

        for (instance, batch) in pending {
            self.send(&instance, &batch).await;
        }
    }

    /// How this deployment is named in a message.
    ///
    /// The instance id is appended only when it is worth appending. A
    /// single-server deployment's messages must not start saying
    /// "myserver / default" because instances exist as a concept.
    fn label(&self, instance: &str) -> String {
        if self.several {
            format!("{} / {instance}", self.hostname)
        } else {
            self.hostname.clone()
        }
    }

    async fn send(&self, instance: &str, batch: &[Event]) {
        if batch.is_empty() {
            return;
        }
        let label = self.label(instance);

        if let Some(url) = &self.discord {
            let payload = discord::payload(batch, &label);
            self.post(url, &payload).await;
        }

        if let Some(url) = &self.generic {
            let payload = serde_json::json!({
                "hostname": self.hostname,
                "instance": instance,
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
        assert!(Notifier::new(&config, "test", false).is_none());

        config.enabled = true;
        config.discord_webhook = None;
        assert!(Notifier::new(&config, "test", false).is_none());
    }

    #[test]
    fn high_frequency_events_are_never_sent() {
        let notifier = Notifier::new(&config(), "test", false).unwrap();

        let noisy = [
            Event::Resources(ResourceSample {
                at: chrono::Utc::now(),
                cpu_percent: 1.0,
                cpu_percent_all_cores: 1.0,
                cpu_core_count: 1,
                memory_bytes: 1,
                process_count: 1,
                host_cpu_percent: 1.0,
                host_memory_percent: 1.0,
                network_rx_bytes_per_sec: 0,
                network_tx_bytes_per_sec: 0,
            }),
            Event::Log(LogLine {
                category: cellar_core::profile::Category::Other,
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

    /// A message has to say which server it is about, and only when it must.
    #[test]
    fn a_message_names_the_instance_only_when_there_is_more_than_one() {
        let one = Notifier::new(&config(), "applejack-01", false).unwrap();
        assert_eq!(one.label("default"), "applejack-01");

        // A single-server deployment's messages must not start saying
        // "applejack-01 / default" because instances exist as a concept.
        let several = Notifier::new(&config(), "applejack-01", true).unwrap();
        assert_eq!(several.label("published"), "applejack-01 / published");
    }

    #[test]
    fn notable_events_are_sent_by_default() {
        let notifier = Notifier::new(&config(), "test", false).unwrap();

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
        let notifier = Notifier::new(&config, "test", false).unwrap();

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
