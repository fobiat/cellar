/* No shell cache, deliberately. A live server view that serves a stale
 * dashboard from disk is worse than one that fails to load: the operator reads
 * a state that was true minutes ago and acts on it. The worker is here for
 * installability and for notification clicks, and nothing else.
 *
 * There used to be an unused `CACHE` constant here that read as an intention. */

self.addEventListener("install", (event) => {
  event.waitUntil(self.skipWaiting());
});

self.addEventListener("activate", (event) => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  event.waitUntil(self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((clients) => {
    const client = clients.find((candidate) => "focus" in candidate);
    return client ? client.focus() : self.clients.openWindow("/");
  }));
});
