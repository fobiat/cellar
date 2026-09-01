/* Cellar's dispatch console.
 *
 * No framework and no build step: the whole page is three files embedded in the
 * binary, because a server manager that needs npm to render its own status page
 * is a server manager with a second thing to keep working.
 */

const $ = (selector) => document.querySelector(selector);
const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

/* Every value from the server is inserted as text, never as HTML. A player's
 * display name is chosen by the account holder and reaches this page through
 * the log; treating it as markup would be a stored cross-site scripting hole
 * with a Steam account as the input field. */
const text = (value) => (value === null || value === undefined ? "" : String(value));

let socket = null;
let cpuHistory = [];
let resourceHistory = [];
let consoleRecords = [];
let consolePaused = false;
let buildDriftState = "";
let activeTab = "dispatch";
let serviceWorker = null;

/* A queue, not a single slot. Killing a server produces several failures at
 * once, and a toast that overwrites the previous one shows the least
 * informative of them. */
const toastQueue = [];
let toastShowing = false;

function showToast(message, kind) {
  toastQueue.push({ message: String(message), kind: kind || "info" });
  if (!toastShowing) drainToasts();
}

/* How long each kind stays. An error is sticky and carries a dismiss button:
 * an error that disappears after 4.5 seconds while the operator is reading the
 * console is an error that never happened as far as they know. */
const TOAST_SECONDS = { info: 4.5, success: 4.5, warn: 9, error: 0 };

function drainToasts() {
  const toast = $("#toast");
  const next = toastQueue.shift();
  if (next === undefined) {
    toastShowing = false;
    toast.hidden = true;
    return;
  }
  toastShowing = true;
  toast.replaceChildren();
  toast.append(el("span", null,
    toastQueue.length ? `${next.message} (+${toastQueue.length} more)` : next.message));
  toast.hidden = false;
  clearTimeout(showToast.timer);

  const seconds = TOAST_SECONDS[next.kind] ?? 4.5;
  if (seconds > 0) {
    showToast.timer = setTimeout(drainToasts, seconds * 1000);
    return;
  }
  const dismiss = el("button", "chip", "dismiss");
  dismiss.onclick = drainToasts;
  toast.append(dismiss);
}

/* An empty pane has to say why it is empty.
 *
 * A blank table and a table whose fetch silently returned nothing look the
 * same, and during an incident the second is what an operator will assume. The
 * text is the point: "no documents" says nothing, "the gamemode has not written
 * any yet" says where to look next. */
function emptyRow(body, columns, why) {
  const row = el("tr");
  const cell = el("td", "muted", why);
  cell.colSpan = columns;
  row.append(cell);
  body.append(row);
}

/* ---- theme --------------------------------------------------------------- */

/* Dark by default, whatever the system says.
 *
 * An operations console that repaints itself white because a phone is in light
 * mode is a console nobody can read outdoors at night, which is when it gets
 * read. So `prefers-color-scheme` is not the default here; it is the third
 * option, chosen deliberately.
 *
 * The light theme was dead until 2026-09-01 for one reason: the `ink` token
 * carried the dark value in both themes, so light mode painted #201F1D body
 * text on a #0E0F11 ground at 1.15:1. Two tokens moved and the palette test
 * now holds both halves to WCAG AA, so this option leads somewhere. */
function applyTheme(choice) {
  if (choice === "system") {
    // Removing the attribute is what lets the generated palette's
    // `prefers-color-scheme` block take over.
    document.documentElement.removeAttribute("data-theme");
  } else {
    document.documentElement.setAttribute("data-theme", choice);
  }
  try {
    localStorage.setItem("cellar-theme", choice);
  } catch {
    // Private browsing, or storage denied. The choice still applies to this
    // page; it just will not survive a reload.
  }
}

function restoreTheme() {
  let choice = "dark";
  try {
    choice = localStorage.getItem("cellar-theme") || "dark";
  } catch {
    // Same as above.
  }
  applyTheme(choice);
  const select = $("#theme");
  if (select) {
    select.value = choice;
    select.onchange = () => applyTheme(select.value);
  }
}

/* Confirming a destructive action, as a dialog rather than window.confirm.
 *
 * `window.confirm` cannot say which server, cannot count who is about to be
 * disconnected, and cannot ask for the name typed back. Some browsers also let
 * a user suppress it permanently, which turns "really stop the production
 * server?" into a silent yes. A native <dialog> brings the focus trap, Escape
 * and the backdrop with it and needs no library.
 *
 * `typed` is the tier-2 guard: pass the string that has to be entered before
 * Confirm becomes available. Everything else is tier 1, and tier 0 does not
 * call this at all.
 */
function confirmAction({ title, body, typed }) {
  return new Promise((resolve) => {
    const dialog = $("#confirm-dialog");
    const input = $("#confirm-typed");
    const go = $("#confirm-go");
    const cancel = $("#confirm-cancel");

    $("#confirm-title").textContent = title;
    $("#confirm-body").textContent = body || "";

    input.hidden = !typed;
    input.value = "";
    input.placeholder = typed ? `type ${typed} to confirm` : "";
    go.disabled = Boolean(typed);
    input.oninput = () => { go.disabled = input.value.trim() !== typed; };

    /* Resolved from the buttons and from `cancel`, never from `close`.
     *
     * A <form method="dialog"> closes the dialog natively, which reads as the
     * obvious way to write this, and it is a trap: the `close` event does not
     * fire in every engine that ships <dialog>. Measured here, in the browser
     * this was driven in: the dialog closed, `returnValue` was `go`, and
     * neither an `onclose` property nor an added `close` listener ran, so the
     * promise hung forever and every confirmed stop silently did nothing.
     *
     * `settle` guards against a second resolution, since Escape fires `cancel`
     * and a click fires its own handler. */
    let settled = false;
    const settle = (answer) => {
      if (settled) return;
      settled = true;
      input.oninput = null;
      go.onclick = null;
      cancel.onclick = null;
      dialog.oncancel = null;
      if (dialog.open) dialog.close();
      resolve(answer);
    };

    go.onclick = () => settle(true);
    cancel.onclick = () => settle(false);
    // Escape. The one path the browser drives rather than the page.
    dialog.oncancel = () => settle(false);

    dialog.showModal();
    (typed ? input : go).focus();
  });
}

/* The load-state contract every pane uses.
 *
 * A pane is loading, or it holds an answer, or it holds a reason it does not.
 * There is no fourth state, and in particular there is no blank one: a fetch
 * that threw used to leave whatever was on screen before, which during a phase
 * of killing processes deliberately is the most misleading thing it could do.
 *
 * `load` takes the node the pane renders into so a failure has somewhere to go
 * that is not only a toast. Pass null for a loader with no single container. */
async function load(what, node, work) {
  if (node) node.setAttribute("aria-busy", "true");
  try {
    const result = await work();
    if (node) node.removeAttribute("data-load-error");
    return result;
  } catch (error) {
    const why = error && error.message ? error.message : String(error);
    showToast(`${what} failed: ${why}`, "error");
    if (node) {
      node.setAttribute("data-load-error", why);
      node.replaceChildren(el("p", "notice", `Could not load ${what}: ${why}`));
    }
    return null;
  } finally {
    if (node) node.removeAttribute("aria-busy");
  }
}

/* Fetch and parse, turning every failure into one thrown error with a message
 * worth reading. `fetch` rejects only on a network fault, so a 500 with a JSON
 * body reached the caller as a successful parse of an error document. */
async function api(path, options) {
  const response = await fetch(path, options);
  const body = await response.json().catch(() => null);
  if (!response.ok) {
    throw new Error((body && body.error) || `${response.status} ${response.statusText}`);
  }
  return body;
}

/* Which instance the dashboard is looking at.
 *
 * `null` means the primary, which is what every route already defaults to when
 * `?instance=` is absent, so a single-server config never sends the parameter
 * and its access log stays as it was. */
let selectedInstance = null;
let knownInstances = [];
let lastStatus = null;
let commandHistory = [];
let historyCursor = 0;

/* What Tab completes from: the gamemode's own palette, plus what has been
 * typed here before. Cellar keeps no list of a gamemode's commands, which is
 * the point of the profile. */
function completions() {
  const current = knownInstances.find((entry) => entry.id === selectedInstance) || knownInstances[0];
  const fromProfile = ((current && current.profile && current.profile.command) || [])
    .map((entry) => entry.command);
  return [...new Set([...fromProfile, ...commandHistory])].sort();
}

function instanceId() {
  return selectedInstance;
}

/* ---- the instance strip -------------------------------------------------- */

/* Draw one tile per declared instance and remember which is selected.
 *
 * Hidden for a single-server config: a selector with one option is furniture
 * that teaches an operator nothing, and every route already defaults to the
 * primary when no instance is named. */
async function loadInstances() {
  const data = await api("/api/instances");
  knownInstances = data.instances || [];

  if (selectedInstance && !knownInstances.some((entry) => entry.id === selectedInstance)) {
    selectedInstance = null;
  }
  /* Deliberately left null for a single-server config, rather than filled in
   * with the primary. Naming the only instance would be correct and would also
   * put `?instance=default` in every request and `#/i/default/` in every
   * bookmark of a deployment that has never heard of instances. Measured: it
   * did exactly that until this line existed. */
  if (!selectedInstance && knownInstances.length > 1) {
    selectedInstance = data.primary || null;
  }

  const strip = $("#instance-strip");
  strip.hidden = knownInstances.length < 2;
  strip.replaceChildren();
  if (strip.hidden) return knownInstances;

  knownInstances.forEach((entry, index) => {
    const tile = el("button", "instance");
    tile.type = "button";
    tile.setAttribute("role", "tab");
    tile.setAttribute("aria-selected", String(entry.id === selectedInstance));
    tile.dataset.instance = entry.id;

    const state = entry.unavailable ? "unavailable" : entry.running ? "running" : "stopped";
    tile.append(el("span", `instance-dot lamp ${state === "running" ? "ok" : "down"}`));
    tile.append(el("span", "instance-id", entry.id));

    /* One muted suffix, and only what is not already obvious.
     *
     * The scope is shown when it differs from the id, because that is the case
     * where deleting a document in the wrong place is possible. A running
     * server says nothing: running is the expectation, and a word for it is a
     * word on every tile forever. Everything else lives in the tooltip. */
    const notes = [];
    if (entry.scope !== entry.id) notes.push(entry.scope);
    if (state !== "running") notes.push(state);
    if (!entry.required) notes.push("optional");
    if (notes.length) tile.append(el("span", "instance-meta", notes.join(" · ")));

    tile.title = [
      `${entry.id}: ${state}`,
      `scope ${entry.scope}`,
      entry.required ? "counted by /readyz" : "not counted by /readyz",
      index < 9 ? `Ctrl+${index + 1}` : null,
    ].filter(Boolean).join("\n");

    tile.onclick = () => selectInstance(entry.id);
    strip.append(tile);
  });

  return knownInstances;
}

/* Switch instance and reload everything that was about the old one.
 *
 * Reloading rather than patching in place, because a partial switch is the
 * failure mode that matters here: a console still streaming the old server
 * under a header naming the new one is how an operator types `quit` into the
 * wrong thing. */
function selectInstance(id) {
  if (id === selectedInstance) return;
  selectedInstance = id;
  writeRoute();
  consoleRecords = [];
  cpuHistory = [];
  resourceHistory = [];
  loadInstances();
  refreshStatus();
  showTab(activeTab);
}

/* Append `?instance=` when one is selected. Every call that is about a
 * particular server goes through this rather than building the query inline,
 * so making a new route instance-aware is one call site rather than a search
 * for string concatenation. */
function forInstance(path) {
  const id = instanceId();
  if (!id) return path;
  return path + (path.includes("?") ? "&" : "?") + "instance=" + encodeURIComponent(id);
}

function alertsEnabled() {
  return localStorage.getItem("cellar.alerts") === "on";
}

function renderAlertButton() {
  $("#notification-toggle").textContent = alertsEnabled() ? "Alerts on" : "Enable alerts";
}

async function enableAlerts() {
  if (!("Notification" in window)) {
    showToast("This browser does not support notifications.", "warn");
    return;
  }

  const permission = await Notification.requestPermission();
  if (permission !== "granted") {
    showToast("Alerts are blocked. Allow notifications in browser settings.", "warn");
    return;
  }

  if ("serviceWorker" in navigator) {
    serviceWorker = await navigator.serviceWorker.register("/service-worker.js");
  }
  localStorage.setItem("cellar.alerts", "on");
  renderAlertButton();
  showToast("Browser alerts enabled for server events.", "success");
}

function notifyOperator(title, body) {
  if (!alertsEnabled() || Notification.permission !== "granted") return;
  if (serviceWorker) {
    serviceWorker.showNotification(title, { body, tag: "cellar-server" });
  } else {
    new Notification(title, { body });
  }
}

/* ---- routing ------------------------------------------------------------- */

/* The location hash is the tab state, not a JS variable.
 *
 * It was a variable, which is why the PWA's two manifest shortcuts both landed
 * on whichever tab happened to be default, why a reload lost the tab, and why
 * a link to "the console on the dev instance" could not be written down at all.
 *
 * `#/i/<id>/<tab>` names both; `#/<tab>` is the primary's. `?tab=` still works
 * and redirects, because it is in the wild in bookmarks and in the manifest. */
function readRoute() {
  const legacy = new URLSearchParams(location.search).get("tab");
  if (legacy && !location.hash) return { instance: null, tab: legacy };

  const parts = location.hash.replace(/^#\/?/, "").split("/").filter(Boolean);
  if (parts[0] === "i" && parts.length >= 2) {
    return { instance: parts[1], tab: parts[2] || "dispatch" };
  }
  return { instance: null, tab: parts[0] || "dispatch" };
}

function writeRoute() {
  const wanted = selectedInstance ? `#/i/${selectedInstance}/${activeTab}` : `#/${activeTab}`;
  if (location.hash !== wanted) {
    /* replaceState, not assignment: switching tabs is not navigation, and a
     * back button that walks an operator through every tab they glanced at is
     * a back button nobody can use to leave. */
    history.replaceState(null, "", wanted);
  }
}

function applyRoute() {
  const route = readRoute();
  if (route.instance && route.instance !== selectedInstance
      && knownInstances.some((entry) => entry.id === route.instance)) {
    selectInstance(route.instance);
    return;
  }
  showTab(TAB_LOADERS[route.tab] || document.getElementById(`tab-${route.tab}`)
    ? route.tab
    : "dispatch");
}

/* ---- tabs --------------------------------------------------------------- */

/* `moveFocus` is false for the initial route and for a hash change, because
 * stealing focus on page load is its own accessibility problem. It is true
 * when the operator picked the tab, which is when they want to be there. */
function showTab(name, moveFocus) {
  activeTab = name;
  writeRoute();
  document.querySelectorAll("nav.tabs button").forEach((button) => {
    const selected = button.dataset.tab === name;
    button.setAttribute("aria-selected", String(selected));
    /* Roving tabindex: the tablist is one stop, and Left/Right move within it.
     * Eleven separate tab stops before the content is why a keyboard user
     * would never reach the console. */
    button.tabIndex = selected ? 0 : -1;
  });
  document.querySelectorAll("main section").forEach((section) => {
    section.hidden = section.id !== `tab-${name}`;
  });

  if (moveFocus) $(`#tab-${name}`)?.focus();

  const pane = TAB_LOADERS[name];
  if (pane) load(pane.what, $(pane.into), pane.run);
}

/* Left, Right, Home and End inside the tab bar, per the WAI-ARIA tabs pattern.
 * Without it the bar is a row of eleven buttons a keyboard user tabs through
 * one at a time to reach anything. */
function tablistKey(event) {
  const buttons = [...document.querySelectorAll("nav.tabs button")];
  const here = buttons.indexOf(event.currentTarget);
  if (here < 0) return;

  const next = {
    ArrowLeft: here - 1,
    ArrowRight: here + 1,
    Home: 0,
    End: buttons.length - 1,
  }[event.key];
  if (next === undefined) return;

  event.preventDefault();
  const wrapped = (next + buttons.length) % buttons.length;
  showTab(buttons[wrapped].dataset.tab);
  buttons[wrapped].focus();
}

/* What each tab loads, where a failure goes, and what to call the thing in the
 * message. One table rather than a try/catch inside each loader, so a loader
 * added later cannot quietly be the one without error handling. */
const TAB_LOADERS = {
  records: { what: "documents", into: "#documents", run: () => loadDocuments() },
  database: { what: "the database", into: "#tables", run: () => loadDatabase() },
  players: { what: "players", into: "#players", run: () => loadPlayers() },
  access: { what: "access", into: "#access-list", run: () => loadAccess() },
  releases: { what: "releases", into: "#versions", run: () => loadReleases() },
  settings: { what: "settings", into: "#settings", run: () => loadSettings() },
  monitoring: { what: "status", into: null, run: () => refreshStatus() },
  configs: {
    what: "profiles",
    into: "#config-list",
    run: async () => { await loadConfigs(); await loadGamemode(); },
  },
  precinct: { what: "the gamemode palette", into: "#precinct-palette", run: () => loadPalette() },
  activity: { what: "activity", into: "#activity", run: () => loadActivity() },
  diagnostics: { what: "diagnostics", into: "#diagnostics-checks", run: () => loadDiagnostics() },
};

/* ---- diagnostics --------------------------------------------------------- */

/* The same checks `cellar doctor` runs, from the same crate.
 *
 * They used to live inside the CLI and print as they went, so the dashboard
 * could not reach them and reimplementing them here would have been a second
 * copy that drifts. They live in `cellar-diagnostics` now and this renders
 * whatever it returns, so a check added later appears here without a change. */
async function loadDiagnostics() {
  const data = await api("/api/diagnostics");

  $("#diagnostics-config").textContent = data.config_path || "the config file";

  const failed = (data.checks || []).filter((check) => check.outcome === "fail").length;
  $("#diagnostics-state").textContent = failed
    ? `${failed} problem${failed === 1 ? "" : "s"}.`
    : "Nothing to fix.";

  renderChecks($("#diagnostics-checks"), data.checks || []);
  renderChecks($("#diagnostics-runtime"), data.runtime || []);
  await loadJobs();

  const unparsed = $("#diagnostics-unparsed");
  unparsed.replaceChildren();
  const seen = (data.unparsed || []).filter((entry) => entry.lines > 0);
  if (!seen.length) {
    unparsed.append(el("p", "muted", "Every line so far has parsed."));
    return;
  }
  for (const entry of seen) {
    const heading = knownInstances.length > 1
      ? `${entry.instance}: ${entry.lines} line(s)`
      : `${entry.lines} line(s)`;
    unparsed.append(el("h3", null, heading));
    const block = el("pre", "log");
    block.textContent = (entry.samples || []).join("\n");
    unparsed.append(block);
  }
}

/* What runs on a timer, when it last ran, and whether it worked.
 *
 * "Run now" answers 202 rather than 200 and does not wait: it nudges the job's
 * own loop, so a job cannot be running twice at once however many operators
 * press the button. The row shows the outcome on the next poll. */
async function loadJobs() {
  const data = await api("/api/jobs");
  const body = $("#jobs");
  body.replaceChildren();

  const jobs = data.jobs || [];
  if (!jobs.length) {
    const row = el("tr");
    const cell = el("td", "muted",
      "This process runs no scheduled jobs. Backups, update checks and event retention are "
      + "each off or unconfigured.");
    cell.colSpan = 6;
    row.append(cell);
    body.append(row);
    return;
  }

  for (const job of jobs) {
    const row = el("tr");

    const name = el("td");
    name.append(el("div", null, job.name));
    name.append(el("div", "muted small", job.description));
    row.append(name);

    row.append(el("td", "muted", everyLabel(job.interval_seconds)));
    /* "never" is the load-bearing word here. A backup job that has never run
     * looks identical to one that ran an hour ago unless the cell says so. */
    row.append(el("td", "muted", job.last_run ? new Date(job.last_run).toLocaleString() : "never"));

    const result = el("td");
    if (job.running) {
      result.append(el("span", "wait lamp", " running"));
    } else if (job.last_ok === null || job.last_ok === undefined) {
      result.append(el("span", "muted", "—"));
    } else {
      result.append(el("span", job.last_ok ? "up lamp" : "down lamp", " "));
      result.append(document.createTextNode(job.last_detail || (job.last_ok ? "ok" : "failed")));
    }
    if (job.failures) result.append(el("div", "muted small", `${job.failures} failure(s) so far`));
    row.append(result);

    row.append(el("td", "muted", job.next_run ? new Date(job.next_run).toLocaleString() : "—"));

    const action = el("td");
    const now = el("button", "chip", "run now");
    now.disabled = job.running;
    now.onclick = () => runJob(job.name);
    action.append(now);
    row.append(action);

    body.append(row);
  }
}

/* Seconds are how the API states an interval, because a number is not
 * ambiguous. A person reading a table wants "every 24 hours". */
function everyLabel(seconds) {
  if (seconds % 86400 === 0) return `${seconds / 86400}d`;
  if (seconds % 3600 === 0) return `${seconds / 3600}h`;
  if (seconds % 60 === 0) return `${seconds / 60}m`;
  return `${seconds}s`;
}

async function runJob(name) {
  const response = await fetch(`/api/jobs/${encodeURIComponent(name)}/run`, { method: "POST" });
  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    showToast(`Could not run ${name}: ${text(data.error) || response.status}`, "error");
    return;
  }
  showToast(`Asked ${name} to run.`, "success");
  // Long enough for a quick job to have finished and short enough to feel
  // like a response. A slow one shows as running until the next load.
  setTimeout(() => load("jobs", $("#jobs"), () => loadJobs()), 1200);
}

/* One row per check, with the verdict as a word and not only as a colour. */
function renderChecks(body, checks) {
  body.replaceChildren();
  if (!checks.length) {
    const row = el("tr");
    const cell = el("td", "muted", "Nothing to report.");
    cell.colSpan = 3;
    row.append(cell);
    body.append(row);
    return;
  }
  for (const check of checks) {
    const row = el("tr");
    row.append(el("td", null, check.label));
    row.append(el("td", "muted", check.instance || "—"));
    const result = el("td");
    const word = { ok: "ok", fail: "FAIL", note: "note" }[check.outcome] || check.outcome;
    result.append(el("span", check.outcome === "fail" ? "down" : "muted", `${word} `));
    result.append(document.createTextNode(check.detail));
    row.append(result);
    body.append(row);
  }
}

/* ---- activity ------------------------------------------------------------ */

/* The audit and the observation record, read back at last.
 *
 * `record_command` has written every console command since the console existed
 * and `record_event` every lifecycle event; neither was ever rendered. No new
 * backend writes were needed for this screen, only a query. */
async function loadActivity() {
  const params = new URLSearchParams();
  const search = $("#activity-search").value.trim();
  const source = $("#activity-source").value;
  const days = $("#activity-days").value;
  if (search) params.set("q", search);
  if (source) params.set("source", source);
  if (days) params.set("days", days);
  params.set("limit", "300");

  const data = await api(forInstance(`/api/activity?${params}`));
  const body = $("#activity");
  body.replaceChildren();

  const entries = data.entries || [];
  $("#activity-state").textContent = entries.length
    ? `${entries.length} entr${entries.length === 1 ? "y" : "ies"}, newest first.`
    : "Nothing recorded for that filter.";

  for (const entry of entries) {
    const row = el("tr");
    row.append(el("td", null, new Date(entry.at).toLocaleString()));
    row.append(el("td", null, entry.source === "operator" ? "operator" : entry.kind));
    row.append(el("td", null, entry.actor || "—"));

    /* The command and its reply in one cell, because a command without what it
     * returned is half an audit entry: "who ran quit" matters less than
     * whether the server took it. */
    const what = el("td");
    what.append(el("div", null, entry.detail || "—"));
    if (entry.reply) what.append(el("div", "muted small", entry.reply.slice(0, 400)));
    /* Which server, when a config has more than one. An audit row that does not
     * say which server it is about is an audit row you cannot act on. */
    if (entry.scope && knownInstances.length > 1) {
      what.append(el("div", "muted small", `scope ${entry.scope}`));
    }
    row.append(what);

    /* An event has no outcome. Putting the scope here because the cell was
     * otherwise empty made the column mean two things, which is worse than a
     * dash. */
    const outcome = entry.ok === null || entry.ok === undefined
      ? "—"
      : entry.ok ? "ok" : "refused";
    row.append(el("td", entry.ok === false ? "down" : "muted", outcome));
    body.append(row);
  }
}

/* ---- the gamemode command palette --------------------------------------- */

/* Was thirteen buttons in index.html naming one gamemode's convars. The
 * gamemode declares them now, so a server Cellar has never heard of gets a
 * palette, and AppleJackRP's lives in AppleJackRP's profile. */
async function loadPalette() {
  const data = await api("/api/instances");
  const target = $("#precinct-palette");
  target.replaceChildren();

  const wanted = instanceId() || data.primary;
  const current = (data.instances || []).find((entry) => entry.id === wanted)
    || (data.instances || [])[0];
  const profile = (current && current.profile) || {};
  const commands = profile.command || [];

  $("#precinct-title").textContent = profile.name
    ? `${profile.name} commands`
    : "Gamemode commands";

  if (!commands.length) {
    target.append(el("p", "muted",
      "This gamemode's profile declares no commands. Add [[command]] entries to it, or type "
      + "into the Dispatch console."));
    return;
  }

  /* Ungrouped entries sort last rather than first: a profile that groups some
   * of its commands and not others is saying the rest are miscellaneous. */
  const groups = new Map();
  for (const entry of commands) {
    const key = entry.group || "";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(entry);
  }
  const ordered = [...groups.keys()].sort((a, b) => (a === "" ? 1 : b === "" ? -1 : a.localeCompare(b)));

  for (const name of ordered) {
    if (name) target.append(el("h3", "muted small", name));
    const row = el("div", "chips");
    for (const entry of groups.get(name)) {
      const button = el("button", "chip", entry.label);
      button.title = entry.command;
      button.onclick = async () => {
        if (entry.confirm && !await confirmAction({ title: `Run ${entry.command}?` })) return;
        runCommand(entry.command);
      };
      row.append(button);
    }
    target.append(row);
  }
}

/* ---- access ------------------------------------------------------------- */

async function loadAccess() {
  const response = await fetch(forInstance("/api/access"));
  const data = await response.json();
  if (!response.ok) {
    $("#access-notice").textContent = text(data.error);
    return;
  }

  setLamp($("#access-gate"), data.invite_only ? "up" : "down", data.invite_only ? "invite-only" : "open");
  const list = $("#access-list");
  list.replaceChildren();
  for (const steamId of data.allowlist || []) {
    const row = el("tr");
    const revoke = el("button", "chip", "revoke");
    revoke.onclick = () => changeAccess({ action: "revoke", steam_id: steamId });
    row.append(el("td", null, text(steamId)), el("td", null, revoke));
    list.append(row);
  }
  if (!list.children.length) {
    const row = el("tr");
    const cell = el("td", "muted", "No invited accounts.");
    cell.colSpan = 2;
    row.append(cell);
    list.append(row);
  }
  $("#access-toggle").textContent = data.invite_only ? "Turn gate off" : "Turn gate on";
  $("#access-toggle").onclick = () => changeAccess({ action: "gate", enabled: !data.invite_only });
}

async function changeAccess(body) {
  const response = await fetch(forInstance("/api/access"), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const data = await response.json();
  $("#access-notice").textContent = response.ok ? "Saved." : text(data.error);
  if (response.ok) loadAccess();
}

/* ---- ordinance: features and settings ----------------------------------- */

async function loadSettings() {
  const featureRows = $("#features");
  const settingRows = $("#settings");
  featureRows.replaceChildren();
  settingRows.replaceChildren();

  const response = await fetch(forInstance("/api/settings"));
  const data = await response.json();

  if (!response.ok) {
    const row = el("tr");
    const cell = el("td", "muted", text(data.error));
    cell.colSpan = 4;
    row.append(cell);
    featureRows.append(row);
    return;
  }

  for (const feature of data.features) {
    const row = el("tr");

    const state = el("td");
    const lamp = el("span", `lamp ${feature.enabled ? "up" : "wait"}`, feature.enabled ? "on" : "off");
    state.append(lamp);
    if (!feature.is_default) state.append(el("span", "muted small", "  (override)"));

    const actions = el("td");
    if (feature.toggle === "core") {
      // Core is not toggleable at all; the gamemode refuses it. Showing a
      // button that always fails would be worse than showing none.
      actions.append(el("span", "muted small", "core"));
    } else {
      const button = el("button", "chip", feature.enabled ? "turn off" : "turn on");
      button.onclick = async () => {
        button.disabled = true;
        await setSetting("feature", feature.id, feature.enabled ? "off" : "on");
        loadSettings();
      };
      actions.append(button);
    }

    row.append(
      el("td", null, `${text(feature.id)}`),
      state,
      el("td", "muted", text(feature.toggle)),
      actions,
    );

    const title = el("tr");
    const titleCell = el("td", "muted small", text(feature.title));
    titleCell.colSpan = 4;
    titleCell.style.borderBottom = "0";
    titleCell.style.paddingTop = "0";
    title.append(titleCell);

    featureRows.append(row);
    if (feature.title) featureRows.append(title);
  }

  for (const setting of data.settings) {
    const row = el("tr");

    const value = el("td");
    value.append(el("span", null, text(setting.value)));
    if (setting.value !== setting.default) value.append(el("span", "muted small", "  (override)"));

    const input = el("input");
    input.type = "text";
    input.value = text(setting.value);
    input.style.cssText = "width:90px;padding:3px 6px";

    const save = el("button", "chip", "set");
    save.onclick = async () => {
      save.disabled = true;
      await setSetting("setting", setting.id, input.value.trim());
      loadSettings();
    };

    const actions = el("td");
    actions.append(input, save);

    row.append(
      el("td", null, text(setting.id)),
      value,
      el("td", "muted", text(setting.default)),
      el("td", "muted", text(setting.bounds)),
      actions,
    );
    settingRows.append(row);
  }
}

async function setSetting(kind, id, value) {
  const response = await fetch(forInstance("/api/settings"), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ kind, id, value }),
  });
  const data = await response.json();

  if (!response.ok) {
    appendLine("error", now(), "cellar", text(data.error));
    return;
  }

  appendLine("echo", now(), "you", `> ${text(data.command)}`);
  for (const line of data.reply || []) appendLine("reply", now(), "reply", text(line));
}

async function exportSettings(format, overrides) {
  const response = await fetch(forInstance(`/api/settings/export?format=${format}&overrides=${overrides}`));
  $("#export").textContent = await response.text();
}

/* ---- releases ----------------------------------------------------------- */

async function loadReleases() {
  const rows = $("#versions");
  rows.replaceChildren();

  const response = await fetch("/api/versions");
  const data = await response.json();

  if (!response.ok) {
    $("#update-state").textContent = text(data.error);
    return;
  }

  const versions = data.versions;
  const add = (label, value) => {
    const row = el("tr");
    row.append(el("td", "muted", label), el("td", null, value));
    rows.append(row);
  };

  const program = data.program_update;
  if (program) {
    add("Cellar", `${text(program.current)}${program.update_available ? `, ${text(program.latest)} available` : ", current"}`);
    if (program.checked_at) add("Cellar checked", text(program.checked_at));
    if (program.error) add("Cellar check", `error: ${text(program.error)}`);
  }

  if (versions.gamemode) {
    add("gamemode", `${text(versions.gamemode.version)} (${text(versions.gamemode.commit)})`);
    add("built", text(versions.gamemode.build_date));
  }
  if (versions.git) {
    const dirty = versions.git.dirty ? ", uncommitted changes" : "";
    add("checkout", `${versions.git.head.slice(0, 8)} on ${text(versions.git.branch) || "detached"}${dirty}`);
    add("remote", versions.git.remote_head ? versions.git.remote_head.slice(0, 8) : "not checked");
  }
  if (versions.engine) {
    add("engine build", text(versions.engine.installed_build));
    add("published", text(versions.engine.available_build) || "not checked");
  }
  for (const problem of versions.problems || []) add("note", problem);

  const state = $("#update-state");
  const what = $("#update-what");
  what.replaceChildren();

  const decision = data.decision || {};
  for (const line of decision.what || []) what.append(el("li", null, text(line)));

  switch (decision.decision) {
    case "up_to_date":
      state.className = "up lamp";
      state.textContent = "Up to date";
      break;
    case "available":
      state.className = "live lamp";
      state.textContent = `An update is available (policy: ${text(data.policy)})`;
      break;
    case "deferred":
      state.className = "wait lamp";
      state.textContent = `Available, deferred: ${text(decision.why)}`;
      break;
    case "apply":
      state.className = "live lamp";
      state.textContent = "Will be applied on the next check";
      break;
    default:
      state.className = "muted";
      state.textContent = "Unknown";
  }

  loadChangelog();
}

async function loadChangelog() {
  const target = $("#changelog");
  target.replaceChildren();

  const response = await fetch("/api/changelog");
  const releases = await response.json();

  if (!Array.isArray(releases) || !releases.length) {
    target.append(el("p", "muted small", "No CHANGELOG.md beside the project."));
    return;
  }

  for (const release of releases) {
    const heading = el("h3");
    heading.style.cssText = "margin:14px 0 6px;font-size:13px;color:var(--aj-azure)";
    heading.textContent = `${release.version}${release.date ? ` · ${release.date}` : ""}`;
    target.append(heading);

    for (const section of release.sections) {
      target.append(el("div", "muted small", section.name));
      const list = el("ul", "small");
      for (const item of section.items) list.append(el("li", null, firstSentence(item)));
      target.append(list);
    }
  }
}

/* The changelog is markdown written by a person. It is rendered as text, never
 * as HTML: the emphasis is stripped rather than interpreted. */
function firstSentence(item) {
  const plain = String(item).replace(/\*\*/g, "").replace(/`/g, "");
  const stop = plain.indexOf(". ");
  const line = stop === -1 ? plain : plain.slice(0, stop + 1);
  return line.length > 200 ? `${line.slice(0, 197)}…` : line;
}

/* ---- status ------------------------------------------------------------- */

async function refreshStatus() {
  let data;
  try {
    const response = await fetch(forInstance("/api/status"));
    if (response.status === 401) return showGate();
    data = await response.json();
  } catch {
    setLamp($("#stat-state"), "down", "unreachable");
    return;
  }

  /* Kept so a confirm dialog can say what a destructive action costs without
   * a second round trip while the operator is waiting on the prompt. */
  lastStatus = data;

  const server = data.server;
  const bridge = data.bridge;
  const game = data.game || "gamemode unknown";
  const profile = data.scope || "profile unknown";
  const mode = data.mode || "mode unknown";
  const modeLabel = mode === "development" ? "LIVE - DEVELOPMENT" : mode === "published" ? "LIVE - PUBLISHED" : mode;
  const supervisor = data.supervisor || {};
  const restartLabel = supervisor.auto_restart_on_crash ? "AUTO-RESTART ON CRASH" : "MANUAL RESTART";

  $("#header-profile").textContent = `${text(modeLabel)} · ${text(game)} · ${text(profile)}`;
  $("#header-profile").title = `Mode: ${text(modeLabel)}, gamemode: ${text(game)}, profile: ${text(profile)}`;
  $("#header-restart").textContent = restartLabel;
  $("#header-restart").title = `Supervisor policy: ${text(supervisor.restart_policy || "unknown")}`;

  const cellar = data.cellar || {};
  const cellarVersion = text(cellar.version || "unknown").replace(/^v/, "");
  const cellarCommit = text(cellar.commit || "unknown");
  $("#header-build").textContent = `Cellar v${cellarVersion} · ${cellarCommit.slice(0, 8)}`;
  $("#header-build").title = `Cellar v${cellarVersion}, build commit ${cellarCommit}`;

  if (!server) {
    setLamp($("#stat-state"), "down", "no server");
  } else {
    const lamps = {
      running: "up",
      starting: "wait",
      stopping: "wait",
      backoff: "wait",
      unhealthy: "warn",
      crash_looping: "down",
      stopped: "down",
    };
    setLamp($("#stat-state"), lamps[server.state] || "wait", stateLabel(server));

    $("#stat-players").textContent = `${server.players.length}/${server.max_players || "-"}`;
    $("#stat-uptime").textContent = formatUptime(server.started_at);

    if (server.resources) {
      $("#stat-memory").textContent = formatBytes(server.resources.memory_bytes);
      const processCpu = processCpuAverage(server.resources);
      $("#stat-cpu").textContent = `${Math.max(0, Math.min(100, processCpu)).toFixed(0)}%`;
      cpuHistory.push(processCpu);
      if (cpuHistory.length > 120) cpuHistory.shift();
      drawSpark($("#spark-cpu"), cpuHistory);
      resourceHistory = server.resource_history || [server.resources];
      const latest = server.resources;
      $("#metric-process-cpu").textContent = percent(processCpuAverage(latest));
      $("#metric-process-cpu-raw").textContent = `${Number(latest.cpu_percent || 0).toFixed(1)}%`;
      $("#metric-cpu-cores").textContent = text(latest.cpu_core_count || "unknown");
      $("#metric-host-cpu").textContent = percent(latest.host_cpu_percent);
      $("#metric-process-memory").textContent = formatBytes(latest.memory_bytes);
      $("#metric-host-memory").textContent = percent(latest.host_memory_percent);
      $("#metric-process-count").textContent = text(latest.process_count || 0);
      $("#metric-network-in").textContent = `${formatBytes(latest.network_rx_bytes_per_sec)}/s`;
      $("#metric-network-out").textContent = `${formatBytes(latest.network_tx_bytes_per_sec)}/s`;
      drawPercentChart($("#spark-resources"), [
        { values: resourceHistory.map(processCpuAverage), className: "chart-process" },
        { values: resourceHistory.map((sample) => sample.host_cpu_percent), className: "chart-host" },
      ]);
      $("#telemetry-state").textContent = `${resourceHistory.length} samples · updates every 2s · process CPU avg across ${latest.cpu_core_count || "unknown"} logical cores`;
    }

    renderRoster(server.players);
    renderTimings(server.status_bar);
    $("#stat-unparsed").textContent = server.unparsed_lines;
  }

  setLamp(
    $("#stat-bridge"),
    bridge.enabled ? (bridge.healthy ? "up" : "down") : "wait",
    bridge.enabled ? (bridge.healthy ? "healthy" : "failing") : "off",
  );
  $("#bridge-detail").textContent =
    `${bridge.reads} read · ${bridge.writes} write · ${bridge.absent} absent · ${bridge.refused} refused`;
  $("#bridge-conflicts").textContent = bridge.would_conflict;
  $("#bridge-error").textContent = text(bridge.last_error);

  const mariadb = data.mariadb;
  if (data.database) {
    setLamp($("#stat-mariadb"), "up", "connected");
  } else if (!mariadb) {
    setLamp($("#stat-mariadb"), "wait", "off");
  } else {
    const lamps = { running: "up", starting: "wait", stopping: "wait", backoff: "wait" };
    setLamp($("#stat-mariadb"), lamps[mariadb.state] || "down", mariadb.state.replace("_", " "));
  }

  const health = data.health || {};
  setLamp($("#stat-map"), health.map ? "up" : "down", health.map ? "loaded" : "check needed");
  renderAddresses(data.addresses);
  renderAntiCheat(data.anti_cheat);
  renderWebAuth(data.web_auth);
  const access = data.access || {};
  setLamp($("#stat-access"), access.invite_only ? "up" : "wait", access.invite_only ? "invite-only" : "public");
}

function renderAntiCheat(status) {
  if (!status) return;
  const lamps = { enabled: "up", disabled: "down", unknown: "wait" };
  setLamp($("#stat-anti-cheat"), lamps[status.state] || "wait", status.state || "unknown");
  setLamp($("#anti-cheat-summary"), lamps[status.state] || "wait", status.summary || "unknown");
  const target = $("#anti-cheat-types");
  if (!target) return;
  target.replaceChildren();
  if (!status.types?.length) {
    target.append(el("p", "muted small", "No known anti-cheat signal was found in the engine log."));
    return;
  }
  for (const type of status.types) {
    const row = el("div", "security-row");
    row.append(el("span", `lamp ${lamps[type.state] || "wait"}`, `${text(type.name)} · ${text(type.state)}`));
    if (type.evidence) row.append(el("code", "muted small", text(type.evidence)));
    target.append(row);
  }
}

function renderWebAuth(auth) {
  const target = $("#web-auth-reminder");
  if (!target || !auth) return;
  const reachable = auth.bind && !auth.bind.startsWith("127.") && !auth.bind.startsWith("localhost") && !auth.bind.startsWith("[::1]");
  if (reachable && !auth.password_configured) {
    target.className = "notice down";
    target.textContent = "Action needed: this listener is reachable off-box without a configured password. Set CELLAR_WEB_PASSWORD_HASH before exposing it.";
  } else if (!reachable && !auth.password_configured) {
    target.className = "notice muted";
    target.textContent = "Reminder: the UI has no password gate. Keep it on loopback, or configure CELLAR_WEB_PASSWORD_HASH before remote access.";
  } else {
    target.className = "notice up";
    target.textContent = `Password authentication is configured for ${text(auth.bind)}.`;
  }
}

async function refreshBuildHealth() {
  const response = await fetch("/api/versions");
  if (!response.ok) return;
  const data = await response.json();
  const drift = data.build_drift || {};
  const lamps = { synced: "up", drifted: "down", unknown: "wait" };
  setLamp($("#stat-build"), lamps[drift.state] || "wait", drift.state === "drifted" ? "out of sync" : drift.state || "unknown");
  const previous = buildDriftState;
  buildDriftState = drift.state || "unknown";
  if (buildDriftState === "drifted" && previous !== "drifted") {
    notifyOperator("AppleJackRP build drift", drift.detail || "The running build differs from origin/main.");
    appendLine("error", now(), "cellar", drift.detail || "AppleJackRP build drift detected.", false, "error", "cellar");
  }
}

const tableTools = [
  ["roster-search", "roster", "roster-sort"],
  ["access-search", "access-list", null],
  ["feature-search", "features", "feature-sort"],
  ["setting-search", "settings", "setting-sort"],
  ["player-search", "players", "player-sort"],
  ["table-search", "tables", "table-sort"],
];

/* Filtering and sorting are user actions, not a refresh concern.
 *
 * This used to be called from `refreshStatus`, on a two second interval.
 * `body.append(row)` on a row that is already attached is a remove and an
 * insert, which blurs anything focused inside it, and the Settings table holds
 * live inputs: typing a convar value there lost focus twice a second. It is
 * called from the search and sort controls now, and it still refuses to
 * reorder a table somebody is typing into. */
function applyTableTools() {
  for (const [inputId, bodyId, sortId] of tableTools) {
    const input = $("#" + inputId);
    const body = $("#" + bodyId);
    if (!input || !body) continue;
    if (body.contains(document.activeElement)) continue;
    const query = input.value.trim().toLowerCase();
    const rows = [...body.children];
    for (const row of rows) row.hidden = Boolean(query) && !row.textContent.toLowerCase().includes(query);
    if (!sortId) continue;
    const column = Number($("#" + sortId)?.value || 0);
    const sortable = rows.filter((row) => row.cells.length > column);
    sortable.sort((a, b) => (a.cells[column]?.textContent || "").localeCompare(b.cells[column]?.textContent || "", undefined, { numeric: true }));
    for (const row of sortable) body.append(row);
  }
}

// An exit with no code was killed by a signal, which is not the same as a
// clean exit and must not read as one.
function describeExit(exit) {
  if (!exit || exit.code === null || exit.code === undefined) return "killed by a signal";
  return exit.code === 0 ? "code 0, cleanly" : `code ${exit.code}`;
}

// A state with no process reads as an absence unless it says how the last run
// ended. Exit 0 after a stop and exit 137 after an OOM kill are the same word
// otherwise.
function stateLabel(server) {
  const word = server.state.replace("_", " ");
  if (!server.last_exit || (server.state !== "stopped" && server.state !== "crash_looping")) {
    return word;
  }
  return `${word}, ${describeExit(server.last_exit)}`;
}

/* The state classes a lamp may hold. Named here so `setLamp` can remove the
 * old one without knowing which it was.
 *
 * It used to assign `className` outright, which was correct for the `#stat-*`
 * lamps and wrong for `#connection-state`: that element starts as
 * `connection lamp wait`, so the first WebSocket open dropped `.connection`,
 * the mobile rule hiding it stopped applying, and the element gained `.value`
 * at 20px. A phone showed a large "live" label that was designed to be
 * invisible. */
const LAMP_STATES = ["up", "down", "wait", "warn", "live"];

function setLamp(node, state, label) {
  node.classList.add("lamp");
  node.classList.remove(...LAMP_STATES.filter((other) => other !== state));
  node.classList.add(state);
  node.textContent = label;
}

function renderTimings(bar) {
  const target = $("#timings");
  target.replaceChildren();
  if (!bar) return;

  const entries = [
    ["network", bar.network_ms],
    ["physics", bar.physics_ms],
    ["navmesh", bar.navmesh_ms],
    ["animation", bar.animation_ms],
    ["update", bar.update_ms],
  ].filter(([, value]) => value !== null && value !== undefined);

  if (!entries.length) {
    target.append(el("p", "muted small", "No frame timings in the status bar yet."));
    return;
  }

  const table = el("table");
  const body = el("tbody");
  for (const [name, value] of entries) {
    const row = el("tr");
    row.append(el("td", null, name), el("td", null, `${value.toFixed(2)} ms`));
    body.append(row);
  }
  table.append(body);
  target.append(table);
}

function renderRoster(players) {
  const body = $("#roster");
  body.replaceChildren();

  if (!players.length) {
    const row = el("tr");
    const cell = el("td", "muted", "Nobody connected.");
    cell.colSpan = 4;
    row.append(cell);
    body.append(row);
    return;
  }

  for (const player of players) {
    const row = el("tr");
    const kick = el("button", "chip", "kick");
    kick.onclick = () => runCommand(`kick ${player.steam_id}`);

    const actions = el("td");
    actions.append(kick);

    row.append(
      el("td", null, text(player.name)),
      el("td", null, text(player.steam_id)),
      el("td", null, formatUptime(player.joined_at)),
      actions,
    );
    body.append(row);
  }
}

/* ---- console ------------------------------------------------------------ */

/* Lines Cellar itself writes have no gamemode category, because they are not
 * the gamemode talking. `cellar` is the honest bucket for them, and it is one
 * the filter already offers. */
function appendLine(kind, at, who, message, live = false, level = "info", category = "cellar") {
  const record = { kind, at, who, message, live, level, category };
  consoleRecords.push(record);
  if (consoleRecords.length > 5000) consoleRecords.shift();

  /* Append one node, rather than tearing the whole console down and rebuilding
   * it. The old version cleared the whole node and re-created up to 1500
   * elements for every single arriving line, which is O(n) DOM work per line
   * and locks the tab under a server that is talking fast. That is also what
   * "slow mode" existed to paper over, so slow mode is gone: the fix is to make
   * rendering cheap, not to render less often and call it a feature.
   *
   * Paused still collects. A pause that dropped lines would be a pause that
   * loses the ones you paused to read around. */
  if (consolePaused) return;
  const node = lineNode(record);
  if (!node) return;

  const console_ = $("#console");
  const pinned = console_.scrollTop + console_.clientHeight >= console_.scrollHeight - 40;
  console_.append(node);
  while (console_.children.length > 1500) console_.firstChild.remove();
  if (pinned) console_.scrollTop = console_.scrollHeight;
}

const LEVEL_RANK = { trace: 0, debug: 1, info: 2, warning: 3, error: 4 };
const LEVELS = ["trace", "debug", "info", "warning", "error"];
const CATEGORIES = ["cellar", "engine", "gameplay", "network", "players", "storage", "other"];

/* Whether the current filters admit this record. One predicate, used by both
 * the incremental append and the full redraw, so a filter cannot apply to only
 * one of them. */
function consoleAdmits(record) {
  const query = text($("#console-filter")?.value).trim().toLowerCase();
  const minimum = $("#console-level")?.value || "";
  const category = $("#console-category")?.value || "";
  const view = $("#console-view")?.value || "all";

  if (query && !`${record.who} ${record.message}`.toLowerCase().includes(query)) return false;
  if (minimum && (LEVEL_RANK[record.level] ?? 2) < LEVEL_RANK[minimum]) return false;
  if (category && record.category !== category) return false;
  if (view === "command" && !["echo", "reply"].includes(record.kind)) return false;
  if (view === "background" && !["log", "join", "leave"].includes(record.kind)) return false;
  if (view === "errors" && record.level !== "error" && record.kind !== "error") return false;
  return true;
}

function lineNode(record) {
  if (!consoleAdmits(record)) return null;
  const level = LEVELS.includes(record.level) ? record.level : "info";
  const category = CATEGORIES.includes(record.category) ? record.category : "other";
  const line = el("div", `line ${record.kind} level-${level} category-${category}`);
  line.append(
    el("span", "at", record.at),
    el("span", "who", record.who),
    el("span", "msg", record.message),
  );
  return line;
}

/* The full redraw, for when the filters change rather than when a line
 * arrives. Still one pass, but now it happens on a click instead of on every
 * line the engine prints. */
function renderConsole() {
  const console_ = $("#console");
  if (consolePaused) return;
  const nodes = [];
  for (const record of consoleRecords) {
    const node = lineNode(record);
    if (node) nodes.push(node);
  }
  console_.replaceChildren(...nodes.slice(-1500));
  console_.scrollTop = console_.scrollHeight;
}

function renderAddresses(addresses) {
  const target = $("#addresses");
  if (!target || !addresses) return;
  target.replaceChildren();
  for (const address of addresses) {
    const row = el("div", "address-row");
    row.append(el("strong", null, address.label));
    row.append(el("span", "muted small", `${address.bind} · ${address.state}`));
    if (address.local_url) row.append(el("code", "small", address.local_url));
    if (address.remote_url) row.append(el("code", "small live", `Tailscale ${address.remote_url}`));
    target.append(row);
  }
}

/* Whether the event stream will render this command for us.
 *
 * `command_dispatched` and `command_replied` are broadcast to every connected
 * browser, so rendering the reply locally as well shows it twice. Rendering it
 * only locally would instead hide every command another operator, the CLI or
 * MCP ran. The stream is the source when there is one; the local copy is the
 * fallback for a dropped socket, where showing nothing would be worse. */
function commandsArriveOnTheStream() {
  return socket && socket.readyState === WebSocket.OPEN;
}

async function runCommand(command) {
  if (!command.trim()) return;
  const echoLocally = !commandsArriveOnTheStream();
  if (echoLocally) appendLine("echo", now(), "you", `> ${command}`);

  try {
    const response = await fetch(forInstance("/api/exec"), {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ command }),
    });
    const data = await response.json();

    if (!response.ok) {
      // Always shown: a refusal never reaches the event stream, because
      // nothing was dispatched.
      appendLine("error", now(), "cellar", text(data.error));
      return;
    }
    if (echoLocally) {
      for (const line of data.reply) appendLine("reply", now(), "reply", text(line));
      if (!data.reply.length) appendLine("reply", now(), "reply", "(no output)");
    }
  } catch (error) {
    appendLine("error", now(), "cellar", String(error));
  }
}

/* ---- live events -------------------------------------------------------- */

/* What the console knows about its own completeness.
 *
 * A console that silently skips lines and still looks complete is worse than
 * one that says it lost some. `missedEvents` counts what the broadcast channel
 * told us it dropped; `backfilled` counts what was recovered from the log file
 * after a reconnect; `lastSeen` is the high-water mark the recovery asks from. */
let missedEvents = 0;
let backfilledLines = 0;
let lastSeen = null;
let hasConnectedOnce = false;

function noteSeen(at) {
  if (!at) return;
  if (!lastSeen || at > lastSeen) lastSeen = at;
}

function renderIntegrity() {
  const node = $("#console-integrity");
  if (!node) return;
  if (!missedEvents && !backfilledLines) {
    node.textContent = "No gaps.";
    node.classList.remove("down");
    return;
  }
  const parts = [];
  if (missedEvents) parts.push(`${missedEvents} event(s) dropped by this browser`);
  if (backfilledLines) parts.push(`${backfilledLines} line(s) recovered from the log`);
  node.textContent = parts.join(", ") + ".";
  node.classList.toggle("down", missedEvents > backfilledLines);
}

/* Fill the hole a dropped socket left, from the log file.
 *
 * The engine's log is the persistent record, so a reconnect can recover what
 * the stream missed rather than resuming mid-gap and looking complete. Only
 * lines strictly after the last one already shown, so nothing is doubled. */
async function backfillSince(mark) {
  if (!mark) {
    /* Nothing to ask from, so the gap cannot be filled. Saying so beats a
     * console that reconnects and looks complete. */
    appendLine("error", now(), "cellar", "--- reconnected, but there is no mark to recover from ---");
    return;
  }
  const params = new URLSearchParams({ since: mark, limit: "500" });
  const data = await api(forInstance(`/api/logs?${params}`));
  const lines = data.lines || [];
  if (!lines.length) return;

  appendLine("echo", now(), "cellar", `--- recovering ${lines.length} line(s) missed while disconnected ---`);
  for (const line of lines) {
    appendLine(line.level === "error" ? "error" : "", clock(line.at), text(line.tag), text(line.message), false, line.level, text(line.category) || "other");
    noteSeen(line.at);
  }
  backfilledLines += lines.length;
  renderIntegrity();
}

function connect() {
  const protocol = location.protocol === "https:" ? "wss" : "ws";
  socket = new WebSocket(`${protocol}://${location.host}${forInstance("/api/events")}`);

  socket.onopen = () => {
    setLamp($("#connection-state"), "up", "live");
    /* Only after a reconnect. On the first connection there is no gap: the
     * initial log load already covers everything before now. */
    if (hasConnectedOnce) {
      load("the missed lines", null, () => backfillSince(lastSeen));
    }
    hasConnectedOnce = true;
  };
  socket.onerror = () => setLamp($("#connection-state"), "down", "error");

  socket.onmessage = (message) => {
    const event = JSON.parse(message.data);
    switch (event.kind) {
      case "log":
        /* The category comes from the server. It used to be recomputed here
         * from a hand-copied regex chain, and the two copies had already
         * diverged: the JavaScript one still tested for `applejack` after the
         * Rust one started asking the gamemode profile. */
        appendLine(event.level === "error" ? "error" : "", clock(event.at), text(event.logger), text(event.message), true, event.level, text(event.category) || "other");
        noteSeen(event.at);
        break;
      case "player_joined":
        appendLine("join", now(), "join", `${text(event.name)} [${event.steam_id}]`);
        refreshStatus();
        break;
      case "player_left":
        appendLine("leave", now(), "left", `${text(event.name)} [${event.steam_id}]`);
        refreshStatus();
        break;
      case "process_started":
      case "server_ready":
        appendLine("echo", now(), "cellar", event.kind.replace("_", " "));
        if (event.kind === "server_ready") {
          notifyOperator("Cellar server ready", event.map ? `Ready on ${event.map}.` : "Ready for players.");
        }
        refreshStatus();
        break;
      case "process_exited":
        appendLine("echo", now(), "cellar", `process exited: ${describeExit(event)}`);
        if (!event.graceful) {
          notifyOperator("Cellar server alert", `The server exited unexpectedly: ${describeExit(event)}.`);
        }
        refreshStatus();
        break;
      /* A command and its reply, as one block.
       *
       * Both of these were broadcast and both fell through to `default`, so a
       * command run from `cellar exec`, from MCP, or by another operator's
       * browser was invisible to anyone watching this one. The console is a
       * shared surface; a second operator typing `quit` into it should not be
       * something you find out about from the exit line. */
      case "command_dispatched":
        appendLine("echo", now(), text(event.actor), `> ${text(event.command)}`);
        break;
      case "command_replied":
        for (const line of event.reply || []) {
          appendLine(event.ok ? "reply" : "error", now(), "reply", text(line));
        }
        if (!(event.reply || []).length) {
          appendLine(event.ok ? "reply" : "error", now(), "reply",
            event.ok ? "(no output)" : "refused");
        }
        break;
      case "bridge_health":
        appendLine(event.healthy ? "echo" : "error", now(), "bridge", text(event.detail));
        break;
      /* Everything the grammar did not recognise, and everything Cellar says
       * about itself: the start timeout, a kill escalation, and the whole
       * shutdown transcript, which graceful_stop publishes as unparsed lines
       * and this used to drop on the floor. A clean shutdown was invisible
       * from the web UI. */
      case "unparsed":
      case "notice":
        appendLine("echo", now(), text(event.origin || "cellar"), text(event.raw));
        break;
      /* A gap, marked as a gap. The alternative is a console that silently
       * skips lines and looks complete. */
      /* A gap, marked as a gap, and counted. The alternative is a console that
       * silently skips lines and looks complete. */
      case "lagged":
        appendLine("error", now(), "cellar", `--- ${event.missed} event(s) missed: this browser fell behind ---`);
        missedEvents += Number(event.missed) || 0;
        renderIntegrity();
        break;
      default:
        break;
    }
  };

  // Reconnect rather than going quietly dead: a dashboard that stops updating
  // without saying so is worse than one that is obviously offline.
  socket.onclose = () => {
    setLamp($("#connection-state"), "down", "reconnecting");
    appendLine("error", now(), "cellar", "--- disconnected: lines from here are recovered on reconnect ---");
    setTimeout(connect, 3000);
  };
}

/* ---- players ------------------------------------------------------------ */

async function loadPlayers() {
  const body = $("#players");
  body.replaceChildren();

  const response = await fetch("/api/players");
  const players = await response.json();

  if (!Array.isArray(players) || !players.length) {
    const row = el("tr");
    const cell = el("td", "muted", "No player history recorded yet.");
    cell.colSpan = 5;
    row.append(cell);
    body.append(row);
    return;
  }

  for (const player of players) {
    const row = el("tr");
    row.append(
      el("td", null, text(player.last_name)),
      el("td", null, text(player.steam_id)),
      el("td", null, formatDuration(player.total_seconds)),
      el("td", null, text(player.sessions)),
      el("td", null, clock(player.last_seen)),
    );
    body.append(row);
  }
}

/* ---- records (the bridge's documents) ----------------------------------- */

async function loadDocuments() {
  const body = $("#documents");
  body.replaceChildren();

  const prefix = $("#doc-prefix").value.trim();
  const response = await fetch(forInstance(`/api/docs?prefix=${encodeURIComponent(prefix)}`));
  const documents = await response.json();

  if (!Array.isArray(documents)) {
    const row = el("tr");
    const cell = el("td", "muted", text(documents.error));
    cell.colSpan = 4;
    row.append(cell);
    body.append(row);
    return;
  }

  if (!documents.length) {
    emptyRow(body, 4, prefix
      ? `No document key starts with '${prefix}'.`
      : "No documents. The gamemode writes these through the bridge as players "
        + "play, so an empty list on a server that has had players means the bridge "
        + "is not being reached.");
    return;
  }

  for (const document_ of documents) {
    const row = el("tr");
    const open = el("button", "chip", "open");
    open.onclick = () => openDocument(document_.key);
    const remove = el("button", "chip", "delete");
    remove.onclick = () => deleteDocument(document_.key);

    const actions = el("td");
    actions.append(open, remove);

    row.append(
      el("td", null, text(document_.key)),
      el("td", null, `r${document_.revision}`),
      el("td", null, formatBytes(document_.bytes)),
      actions,
    );
    body.append(row);
  }
}

async function openDocument(key) {
  const response = await fetch(forInstance(`/api/docs/${key}`));
  const data = await response.json();

  $("#doc-title").textContent = key;
  $("#doc-body").textContent = JSON.stringify(data.document.body, null, 2);

  const history = $("#doc-history");
  history.replaceChildren();
  if (!(data.revisions || []).length) {
    emptyRow(history, 3, "No revisions kept for this document.");
    return;
  }
  for (const revision of data.revisions) {
    const row = el("tr");
    row.append(
      el("td", null, `r${revision.revision}`),
      el("td", null, clock(revision.written_at)),
      el("td", null, text(revision.written_by) || "—"),
    );
    history.append(row);
  }
}

/* ---- database ----------------------------------------------------------- */

async function loadTables() {
  const list = $("#tables");
  list.replaceChildren();

  const response = await fetch("/api/db/tables");
  const tables = await response.json();
  if (!Array.isArray(tables)) {
    list.append(el("p", "muted", text(tables.error) || "The database did not answer."));
    return;
  }
  if (!tables.length) {
    list.append(el("p", "muted",
      "No tables. The grant this connection uses decides what is visible, so an empty "
      + "list can mean an empty schema or a grant that cannot see it."));
    return;
  }

  for (const table of tables) {
    const button = el("button", "chip", `${table.name} · ${table.rows}`);
    button.onclick = () => browseTable(table.name);
    list.append(button);
  }
}

async function loadDatabase() {
  const response = await fetch("/api/db/info");
  const info = await response.json();
  if (response.ok) {
    $("#db-connection").textContent = info.connected ? "connected" : "offline";
    $("#db-owner").textContent = text(info.schema_owner || "unknown");
    $("#db-table-count").textContent = text(info.table_count ?? "—");
    $("#db-size").textContent = formatBytes(info.bytes);
    $("#db-version").textContent = info.server_version
      ? `${text(info.database)} · ${text(info.server_version)} · ${text(info.source || "external")} source · schema supplied by the gamemode`
      : "The gamemode owns the schema. Cellar only inspects it.";
  } else {
    $("#db-connection").textContent = "unavailable";
    $("#db-version").textContent = text(info.error);
  }
  loadTables();
}

async function browseTable(name) {
  const response = await fetch(`/api/db/table/${encodeURIComponent(name)}?limit=50`);
  const data = await response.json();
  $("#db-title").textContent = name;
  renderResult(data.result);
}

async function runQuery() {
  const sql = $("#sql").value;
  const response = await fetch("/api/db/query", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ sql }),
  });
  const data = await response.json();

  if (!response.ok) {
    $("#db-title").textContent = "refused";
    $("#db-notice").textContent = text(data.error);
    renderResult({ columns: [], rows: [] });
    return;
  }

  $("#db-notice").textContent = "";
  $("#db-title").textContent = "query result";
  renderResult(data);
}

function renderResult(result) {
  const head = $("#db-head");
  const body = $("#db-body");
  head.replaceChildren();
  body.replaceChildren();

  if (!result || !result.columns.length) return;

  const headRow = el("tr");
  for (const column of result.columns) headRow.append(el("th", null, column));
  head.append(headRow);

  for (const row of result.rows) {
    const tr = el("tr");
    for (const cell of row) {
      tr.append(cell === null ? el("td", "null", "NULL") : el("td", "wide", cell));
    }
    body.append(tr);
  }

  if (result.truncated) {
    const row = el("tr");
    const cell = el("td", "muted", `Showing the first rows only.`);
    cell.colSpan = result.columns.length;
    row.append(cell);
    body.append(row);
  }
}

/* ---- formatting --------------------------------------------------------- */

const now = () => new Date().toTimeString().slice(0, 8);
const clock = (iso) => (iso ? new Date(iso).toTimeString().slice(0, 8) : "");

// Decimal, matching how disks and dashboards are labelled everywhere else.
function formatBytes(bytes) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = Number(bytes) || 0;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return unit === 0 ? `${value} B` : `${value.toFixed(1)} ${units[unit]}`;
}

function processCpuAverage(sample) {
  const normalized = Number(sample.cpu_percent_all_cores);
  if (Number.isFinite(normalized)) return normalized;
  const raw = Number(sample.cpu_percent) || 0;
  const cores = Math.max(1, Number(sample.cpu_core_count) || 1);
  return raw / cores;
}

function percent(value) {
  return `${Math.max(0, Math.min(100, Number(value) || 0)).toFixed(1)}%`;
}

function formatDuration(seconds) {
  seconds = Number(seconds) || 0;
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (hours) return `${hours}h${String(minutes).padStart(2, "0")}m`;
  if (minutes) return `${minutes}m`;
  return `${seconds}s`;
}

const formatUptime = (iso) =>
  iso ? formatDuration(Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000)) : "—";

function drawSpark(svg, values) {
  drawPercentChart(svg, [{ values, className: "chart-process" }], true);
}

function drawPercentChart(svg, series, compact = false) {
  svg.replaceChildren();
  const view = svg.viewBox.baseVal;
  const width = view.width || (compact ? 320 : 640);
  const height = view.height || (compact ? 64 : 154);
  const left = compact ? 24 : 32;
  const right = width - 4;
  const top = 7;
  const bottom = height - (compact ? 10 : 13);
  const chartHeight = bottom - top;
  const ns = "http://www.w3.org/2000/svg";

  for (const value of [0, 25, 50, 75, 100]) {
    const y = bottom - (value / 100) * chartHeight;
    const grid = document.createElementNS(ns, "line");
    grid.setAttribute("x1", left);
    grid.setAttribute("x2", right);
    grid.setAttribute("y1", y.toFixed(1));
    grid.setAttribute("y2", y.toFixed(1));
    grid.setAttribute("class", "chart-grid");
    svg.append(grid);
    if (value === 0 || value === 50 || value === 100) {
      const label = document.createElementNS(ns, "text");
      label.setAttribute("x", "1");
      label.setAttribute("y", (y + 3).toFixed(1));
      label.setAttribute("class", "chart-label");
      label.textContent = `${value}%`;
      svg.append(label);
    }
  }

  for (const item of series) {
    if (item.values.length < 2) continue;
    const points = item.values.map((value, index) => {
      const x = left + (index / (item.values.length - 1)) * (right - left);
      const bounded = Math.max(0, Math.min(100, Number(value) || 0));
      const y = bottom - (bounded / 100) * chartHeight;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    }).join(" ");
    const line = document.createElementNS(ns, "polyline");
    line.setAttribute("points", points);
    line.setAttribute("fill", "none");
    line.setAttribute("class", item.className || "chart-process");
    line.setAttribute("stroke-width", compact ? "1.5" : "2");
    svg.append(line);
  }
}

/* ---- sign in ------------------------------------------------------------ */

function showGate() {
  $("#gate").hidden = false;
  $("#app").hidden = true;
}

async function signIn(event) {
  event.preventDefault();
  const response = await fetch("/api/login", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ password: $("#password").value }),
  });

  if (response.ok) {
    $("#gate").hidden = true;
    $("#app").hidden = false;
    start();
  } else {
    $("#gate-notice").textContent = "That password was not accepted.";
  }
}

/* ---- boot --------------------------------------------------------------- */

let started = false;

/* Nothing fails quietly.
 *
 * An async loader whose fetch rejects produces an unhandled rejection and no
 * other trace, which is exactly what a killed server causes and exactly what
 * an operator watching for the kill needs to be told about. The periodic
 * refreshes below stay quiet after the first complaint so a server that is
 * down for a minute does not produce thirty toasts. */
function watchForSilentFailures() {
  let lastComplaint = 0;
  const complain = (why) => {
    const at = Date.now();
    if (at - lastComplaint < 10000) return;
    lastComplaint = at;
    showToast(`Something went wrong: ${why}`, "error");
  };
  window.addEventListener("error", (event) => complain(event.message || "script error"));
  window.addEventListener("unhandledrejection", (event) => {
    const reason = event.reason;
    complain(reason && reason.message ? reason.message : String(reason));
  });
}

async function start() {
  if (started) return;
  started = true;
  watchForSilentFailures();
  /* Before anything else: the route names an instance, and every later fetch
   * is about whichever one this settles on. */
  const route = readRoute();
  await load("the instance list", $("#instance-strip"), async () => {
    await loadInstances();
    if (route.instance && knownInstances.some((entry) => entry.id === route.instance)) {
      selectedInstance = route.instance;
      await loadInstances();
    }
  });
  showTab(document.getElementById(`tab-${route.tab}`) ? route.tab : "dispatch");
  window.addEventListener("hashchange", applyRoute);
  // Null: a failure here must not replace the console, which is where the
  // operator is reading the very lines that explain the failure.
  load("the log", null, () => loadLogs());
  connect();
  refreshStatus();
  load("releases", $("#versions"), () => loadReleases());
  refreshBuildHealth();
  setInterval(() => {
    refreshStatus();
    if (activeTab === "database") loadDatabase();
  }, 2000);
  /* Slower than the status poll: the strip changes when a server starts or
   * dies, not every two seconds, and each tick is a whole config read. */
  setInterval(() => loadInstances().catch(() => {}), 10000);
  setInterval(refreshBuildHealth, 30000);
  renderAlertButton();
  if (alertsEnabled() && "serviceWorker" in navigator) {
    navigator.serviceWorker.register("/service-worker.js").then((registration) => {
      serviceWorker = registration;
    }).catch(() => showToast("Alerts could not be restored.", "warn"));
  }
}

async function runRelease(action) {
  const response = await fetch(`/api/release/${action}`, { method: "POST" });
  const data = await response.json();
  $("#release-notice").textContent = response.ok ? `${action} completed.` : text(data.output || data.error);
  for (const line of String(data.output || "").split("\n").filter(Boolean)) appendLine("reply", now(), action, line);
  loadReleases();
}

async function loadLogs() {
  const response = await fetch(forInstance("/api/logs?limit=250"));
  if (!response.ok) return;
  const data = await response.json();
  for (const line of data.lines || []) {
    appendLine(line.level === "error" ? "error" : "", clock(line.at), line.tag, line.message, false, line.level, line.category);
    /* Seeds the high-water mark. Without this a reconnect on a quiet server
     * has nothing to ask from, because only live log events had been marking
     * it, and a server that has not spoken since the page opened has sent
     * none. The gap was then reported and never filled. */
    noteSeen(line.at);
  }
  $("#console-state").textContent = `${data.lines?.length || 0} recent lines · ${data.scanned_files || 0} persistent log file(s)`;
}

async function importSettings(apply) {
  const file = $("#settings-import-file").files?.[0];
  if (!file) {
    $("#settings-import-notice").textContent = "Choose a TOML or YAML settings file first.";
    return;
  }
  if (apply && !await confirmAction({
    title: `Apply settings from ${file.name}?`,
    body: "Every convar in the file is set on the running server.",
    typed: file.name,
  })) return;
  const response = await fetch(forInstance("/api/settings/import"), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ contents: await file.text(), apply }),
  });
  const data = await response.json();
  if (!response.ok) {
    $("#settings-import-notice").textContent = text(data.error);
    return;
  }
  const changes = data.changes || [];
  $("#settings-import-notice").textContent = apply
    ? `Applied ${data.applied?.length || 0} change(s), ${data.failed?.length || 0} failed.`
    : `${changes.length} change(s) found. Review them, then apply.`;
  const plan = $("#settings-import-plan");
  plan.replaceChildren();
  for (const change of changes) {
    const line = el("div", change.refused ? "down" : "muted");
    line.textContent = `${text(change.id)}: ${text(change.from)} -> ${text(change.to)}${change.refused ? ` (${text(change.refused)})` : ""}`;
    plan.append(line);
  }
  for (const item of data.applied || []) {
    for (const line of item.reply || []) appendLine("reply", now(), "import", text(line));
  }
  if (apply) loadSettings();
}

async function scanLogs() {
  const params = new URLSearchParams({ limit: "5000" });
  const query = $("#console-filter").value.trim();
  const level = $("#console-level").value;
  const category = $("#console-category").value;
  if (query) params.set("q", query);
  if (level) params.set("level_min", level);
  if (category) params.set("category", category);
  const response = await fetch(forInstance(`/api/logs?${params}`));
  const data = await response.json();
  if (!response.ok) return showToast(text(data.error), "error");
  consoleRecords = [];
  for (const line of data.lines || []) {
    appendLine(line.level === "error" ? "error" : "", clock(line.at), line.tag, line.message, false, line.level, line.category);
    /* Seeds the high-water mark. Without this a reconnect on a quiet server
     * has nothing to ask from, because only live log events had been marking
     * it, and a server that has not spoken since the page opened has sent
     * none. The gap was then reported and never filled. */
    noteSeen(line.at);
  }
  $("#console-state").textContent = `${data.matched} matches across ${data.scanned_files} persistent log file(s), ${data.scanned_lines} lines scanned`;
}

async function loadConfigs() {
  const target = $("#config-list");
  const response = await fetch("/api/configs");
  const data = await response.json();
  target.replaceChildren();
  if (!response.ok) {
    target.append(el("p", "notice", text(data.error)));
    return;
  }
  const profiles = data.profiles || [];
  if (!profiles.length) {
    target.append(el("p", "muted",
      "No sibling profiles. Cellar looks for other cellar.toml files beside the active "
      + "one; with a single config there is nothing to switch between."));
  }
  const modeActions = $("#config-mode-actions");
  modeActions.replaceChildren();
  for (const mode of ["development", "published"]) {
    const profile = profiles.find((candidate) => candidate.mode === mode);
    const label = mode === "development" ? "Use Development mode" : "Use Published mode";
    const button = el("button", `action ${profile?.active ? "live" : ""}`, profile ? label : `${label} unavailable`);
    button.disabled = !profile || profile.active || Boolean(profile.refusal);
    button.title = !profile
      ? "Install or copy the matching AppleJackRP profile beside the active config"
      : profile.refusal || `Switch to ${profile.name}`;
    if (profile) button.onclick = () => activateConfig(profile.name);
    modeActions.append(button);
  }

  /* Refusals are shown next to the profile, not after the click.
   *
   * Every one of these is a fact about the file that was knowable before the
   * operator chose it: a different web bind, a different log path, a second
   * instance making the whole question ambiguous. Refusing afterwards taught
   * nothing except that the attempt failed. */
  for (const profile of profiles) {
    const mode = profile.mode === "published" ? "Published" : "Development";
    const targetName = profile.game || profile.project || "local project";
    const row = el("div", "config-row");
    const button = el("button", `chip ${profile.active ? "live" : ""}`, `${mode} · ${profile.name} · ${targetName}`);
    button.disabled = profile.active || Boolean(profile.refusal);
    button.title = profile.refusal || profile.path;
    button.onclick = () => activateConfig(profile.name);
    row.append(button);
    if (profile.refusal) row.append(el("span", "muted small", profile.refusal));
    target.append(row);
  }
}

/* What the gamemode said about itself. Every row here was hardcoded to
 * AppleJackRP before `[profile]` existed. */
async function loadGamemode() {
  const data = await api("/api/instances");
  const wanted = instanceId() || data.primary;
  const current = (data.instances || []).find((entry) => entry.id === wanted)
    || (data.instances || [])[0];
  const profile = (current && current.profile) || {};
  const server = (current && current.server) || {};

  $("#gamemode-title").textContent = profile.name || "Gamemode";

  const body = $("#gamemode");
  body.replaceChildren();

  const rows = [
    ["Readiness line", server.ready_pattern || "not set",
      "The log line that means serving. A line this gamemode never logs is a server that "
      + "starts, binds its ports and never passes /readyz."],
    ["Convar prefix", profile.convar_prefix || "not set",
      "Drives the palette's find, the log categories and the settings tab."],
    ["Maps", (profile.map || []).join(", ") || "not declared",
      "A map is a package, passed as the second positional argument to +game. There is no "
      + "+map switch."],
    ["Palette commands", String((profile.command || []).length), ""],
    ["Doctor checks", String((profile.check || []).length), ""],
  ];

  for (const [label, value, note] of rows) {
    const row = el("tr");
    row.append(el("td", null, label));
    const cell = el("td");
    cell.append(el("div", value === "not set" || value === "not declared" ? "muted" : null, value));
    if (note) cell.append(el("div", "muted small", note));
    row.append(cell);
    body.append(row);
  }
}

async function activateConfig(name) {
  const going = await confirmAction({
    title: `Switch to ${name}?`,
    body: "The supervised server will restart, and every connected player will be disconnected.",
    typed: name,
  });
  if (!going) return;
  const response = await fetch("/api/configs/activate", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name }),
  });
  const data = await response.json();
  $("#config-notice").textContent = response.ok ? `Switched to ${name}.` : text(data.error);
  if (response.ok) loadConfigs();
}

document.addEventListener("DOMContentLoaded", async () => {
  document.querySelectorAll("nav.tabs button").forEach((button) => {
    button.onclick = () => showTab(button.dataset.tab, true);
    button.addEventListener("keydown", tablistKey);
  });

  restoreTheme();
  $("#login").onsubmit = signIn;

  document.addEventListener("keydown", globalKeys);
  $("#palette-input").addEventListener("input", () => { paletteCursor = 0; renderPalette(); });
  $("#palette-input").addEventListener("keydown", paletteKey);
  $("#palette").addEventListener("click", (event) => {
    if (event.target === $("#palette")) closePalette();
  });

  /* History and completion.
   *
   * A console with no history is a console where a mistyped long command is
   * retyped from scratch, and completion comes from the gamemode's own profile
   * rather than a list Cellar maintains, so it is right for a gamemode nobody
   * anticipated. */
  $("#command").addEventListener("keydown", (event) => {
    const input = $("#command");
    if (event.key === "Enter") {
      const command = input.value;
      if (command.trim()) {
        commandHistory = commandHistory.filter((entry) => entry !== command);
        commandHistory.push(command);
        if (commandHistory.length > 100) commandHistory.shift();
        localStorage.setItem("cellar.console.history", JSON.stringify(commandHistory));
      }
      historyCursor = commandHistory.length;
      runCommand(command);
      input.value = "";
      return;
    }
    if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      if (!commandHistory.length) return;
      event.preventDefault();
      historyCursor += event.key === "ArrowUp" ? -1 : 1;
      historyCursor = Math.max(0, Math.min(commandHistory.length, historyCursor));
      input.value = commandHistory[historyCursor] || "";
      input.setSelectionRange(input.value.length, input.value.length);
      return;
    }
    if (event.key === "Tab") {
      const prefix = input.value.trim();
      if (!prefix) return;
      const match = completions().find((candidate) => candidate.startsWith(prefix) && candidate !== prefix);
      if (!match) return;
      event.preventDefault();
      input.value = match;
    }
  });

  document.querySelectorAll(".chip[data-command]").forEach((chip) => {
    chip.onclick = () => runCommand(chip.dataset.command);
  });

  $("#doc-search").onclick = loadDocuments;
  $("#activity-refresh").onclick = () => load("activity", $("#activity"), () => loadActivity());
  $("#activity-search").addEventListener("keydown", (event) => {
    if (event.key === "Enter") load("activity", $("#activity"), () => loadActivity());
  });
  for (const control of ["#activity-source", "#activity-days"]) {
    $(control).addEventListener("change", () => load("activity", $("#activity"), () => loadActivity()));
  }
  $("#diagnostics-refresh").onclick = () =>
    load("diagnostics", $("#diagnostics-checks"), () => loadDiagnostics());
  $("#logout").onclick = signOut;
  $("#cellar-exit").onclick = exitCellar;
  $("#run-query").onclick = runQuery;
  $("#release-build").onclick = () => runRelease("build");
  $("#release-publish").onclick = () => runRelease("publish");
  $("#access-add").onclick = () => {
    const steamId = $("#access-steam-id").value.trim();
    if (!steamId) return;
    changeAccess({ action: "allow", steam_id: steamId });
    $("#access-steam-id").value = "";
  };

  $("#stop").onclick = () => control("stop");
  $("#restart").onclick = () => control("restart");
  $("#notification-toggle").onclick = enableAlerts;
  $("#console-filter").addEventListener("input", () => {
    localStorage.setItem("cellar.console.filter", $("#console-filter").value);
    renderConsole();
  });
  $("#console-view").addEventListener("change", () => {
    localStorage.setItem("cellar.console.view", $("#console-view").value);
    renderConsole();
  });
  $("#console-level").addEventListener("change", () => {
    localStorage.setItem("cellar.console.level", $("#console-level").value);
    renderConsole();
  });
  $("#console-pause").onclick = () => {
    consolePaused = !consolePaused;
    $("#console-pause").textContent = consolePaused ? "resume" : "pause";
    $("#console-state").textContent = consolePaused ? "Paused view. Incoming lines are still retained." : "Live output.";
    renderConsole();
  };
  $("#console-scan").onclick = scanLogs;
  $("#console-clear").onclick = () => { consoleRecords = []; renderConsole(); };
  try {
    commandHistory = JSON.parse(localStorage.getItem("cellar.console.history") || "[]");
  } catch {
    commandHistory = [];
  }
  historyCursor = commandHistory.length;
  $("#console-filter").value = localStorage.getItem("cellar.console.filter") || "";
  $("#console-level").value = localStorage.getItem("cellar.console.level") || "";
  $("#console-view").value = localStorage.getItem("cellar.console.view") || "all";
  $("#settings-import-preview").onclick = () => importSettings(false);
  $("#settings-import-apply").onclick = () => importSettings(true);
  for (const [inputId, , sortId] of tableTools) {
    $("#" + inputId)?.addEventListener("input", applyTableTools);
    $("#" + sortId)?.addEventListener("change", applyTableTools);
  }

  const probe = await fetch(forInstance("/api/status"));
  if (probe.status === 401) {
    showGate();
  } else {
    $("#app").hidden = false;
    start();
  }
});

/* ---- the command palette ------------------------------------------------- */

/* Ctrl-K. Three kinds of entry in one list: a tab to jump to, an instance to
 * switch to, and a command from the gamemode's profile to run.
 *
 * This is where the Precinct tab went. A tab existed only because there was
 * nowhere to put buttons; a palette is where a command belongs, and it costs
 * nothing per gamemode because the entries come from the profile. */
let paletteEntries = [];
let paletteCursor = 0;

function paletteCandidates() {
  const entries = [];

  for (const button of document.querySelectorAll("nav.tabs button")) {
    entries.push({
      kind: "tab",
      label: button.textContent.trim(),
      hint: "tab",
      run: () => showTab(button.dataset.tab),
    });
  }

  if (knownInstances.length > 1) {
    for (const instance of knownInstances) {
      entries.push({
        kind: "instance",
        label: instance.id,
        hint: `server . ${instance.scope}`,
        run: () => selectInstance(instance.id),
      });
    }
  }

  const current = knownInstances.find((entry) => entry.id === selectedInstance) || knownInstances[0];
  for (const command of (current && current.profile && current.profile.command) || []) {
    entries.push({
      kind: "command",
      label: command.label,
      hint: command.command,
      run: async () => {
        if (command.confirm && !await confirmAction({ title: `Run ${command.command}?` })) return;
        showTab("dispatch");
        runCommand(command.command);
      },
    });
  }

  return entries;
}

function openPalette() {
  paletteEntries = paletteCandidates();
  paletteCursor = 0;
  $("#palette").hidden = false;
  $("#palette-input").value = "";
  renderPalette();
  $("#palette-input").focus();
}

function closePalette() {
  $("#palette").hidden = true;
  $("#command").focus({ preventScroll: true });
}

function paletteMatches() {
  const needle = $("#palette-input").value.trim().toLowerCase();
  if (!needle) return paletteEntries;
  return paletteEntries.filter((entry) =>
    entry.label.toLowerCase().includes(needle) || entry.hint.toLowerCase().includes(needle));
}

function renderPalette() {
  const list = $("#palette-list");
  const matches = paletteMatches();
  if (paletteCursor >= matches.length) paletteCursor = Math.max(0, matches.length - 1);
  list.replaceChildren();

  if (!matches.length) {
    list.append(el("li", "muted", "Nothing matches."));
    return;
  }

  matches.forEach((entry, index) => {
    const item = el("li", index === paletteCursor ? "selected" : "");
    item.setAttribute("role", "option");
    item.setAttribute("aria-selected", String(index === paletteCursor));
    item.append(el("span", "palette-label", entry.label));
    item.append(el("span", "palette-hint muted small", entry.hint));
    item.onclick = () => { closePalette(); entry.run(); };
    list.append(item);
  });
}

function paletteKey(event) {
  if (event.key === "Escape") { closePalette(); return; }
  if (event.key === "ArrowDown") { paletteCursor += 1; renderPalette(); event.preventDefault(); return; }
  if (event.key === "ArrowUp") { paletteCursor = Math.max(0, paletteCursor - 1); renderPalette(); event.preventDefault(); return; }
  if (event.key === "Enter") {
    const chosen = paletteMatches()[paletteCursor];
    closePalette();
    if (chosen) chosen.run();
    event.preventDefault();
  }
}

/* Ctrl-K opens it; Ctrl-1 through Ctrl-9 selects instance N, which is the
 * gesture someone running three servers will actually reach for. */
function globalKeys(event) {
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
    event.preventDefault();
    if ($("#palette").hidden) openPalette(); else closePalette();
    return;
  }
  if ((event.ctrlKey || event.metaKey) && /^[1-9]$/.test(event.key)) {
    const wanted = knownInstances[Number(event.key) - 1];
    if (wanted) { event.preventDefault(); selectInstance(wanted.id); }
  }
}

/* Confirming a stop or restart names the server and counts who is on it.
 *
 * "Really stop the server?" is a question an operator can only answer wrongly:
 * it does not say which server, and with two instances that is the whole
 * decision, nor how many people are about to be disconnected. Both come from
 * the snapshot that is already on screen. */
async function control(action) {
  const server = lastStatus && lastStatus.server;
  const players = (server && server.players && server.players.length) || 0;
  const named = selectedInstance && knownInstances.length > 1
    ? `the '${selectedInstance}' server`
    : "the server";
  const cost = players === 0
    ? "Nobody is connected."
    : `${players} ${players === 1 ? "player is" : "players are"} connected and will be disconnected.`;

  const going = await confirmAction({
    title: `Really ${action} ${named}?`,
    body: cost,
    // Typing the id back is the tier-2 guard, and only earned when there is
    // more than one server to confuse.
    typed: knownInstances.length > 1 ? selectedInstance : undefined,
  });
  if (!going) return;

  /* Reading the response, at last. This used to await the fetch and drop it, so
   * a refused stop and a successful one looked identical: nothing happened on
   * screen either way until the next two second poll. */
  const response = await fetch(forInstance(`/api/control/${action}`), { method: "POST" });
  if (response.ok) {
    showToast(`Asked ${named} to ${action}.`, "success");
  } else {
    const data = await response.json().catch(() => ({}));
    showToast(`Could not ${action} ${named}: ${text(data.error) || response.status}`, "error");
  }
  refreshStatus();
}

/* ---- the three endpoints that had no way in ------------------------------ */

/* Ending the session. `POST /api/logout` has existed since the gate did and
 * nothing in the UI called it, so the only way out was to close the browser
 * and wait for the cookie to expire. */
async function signOut() {
  await fetch("/api/logout", { method: "POST" });
  location.reload();
}

/* Ending the process. Tier 2, because it stops every supervised server. */
async function exitCellar() {
  const going = await confirmAction({
    title: "Shut down Cellar?",
    body: "Every supervised server is stopped and this process ends. Under a service manager "
      + "or Kubernetes it will be restarted.",
    typed: "shut down",
  });
  if (!going) return;
  const response = await fetch("/api/control/exit", { method: "POST" });
  showToast(
    response.ok ? "Cellar is shutting down." : "Cellar refused the shutdown.",
    response.ok ? "warn" : "error",
  );
}

/* Deleting a document. Tier 1: it names the key, and a document is one row of
 * the bridge's store rather than a server full of players. */
async function deleteDocument(key) {
  const going = await confirmAction({
    title: `Delete ${key}?`,
    body: "The gamemode writes this document back the next time it saves, unless it has "
      + "stopped caring about it.",
  });
  if (!going) return;

  const response = await fetch(forInstance(`/api/docs/${key}`), { method: "DELETE" });
  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    showToast(`Could not delete ${key}: ${text(data.error) || response.status}`, "error");
    return;
  }
  showToast(`Deleted ${key}.`, "success");
  loadDocuments();
}
