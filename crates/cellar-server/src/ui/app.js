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
let consoleSlow = false;
let consoleRenderTimer = null;
let buildDriftState = "";
let activeTab = "dispatch";
let serviceWorker = null;

function showToast(message) {
  const toast = $("#toast");
  toast.textContent = message;
  toast.hidden = false;
  clearTimeout(showToast.timer);
  showToast.timer = setTimeout(() => { toast.hidden = true; }, 4500);
}

function alertsEnabled() {
  return localStorage.getItem("cellar.alerts") === "on";
}

function renderAlertButton() {
  $("#notification-toggle").textContent = alertsEnabled() ? "Alerts on" : "Enable alerts";
}

async function enableAlerts() {
  if (!("Notification" in window)) {
    showToast("This browser does not support notifications.");
    return;
  }

  const permission = await Notification.requestPermission();
  if (permission !== "granted") {
    showToast("Alerts are blocked. Allow notifications in browser settings.");
    return;
  }

  if ("serviceWorker" in navigator) {
    serviceWorker = await navigator.serviceWorker.register("/service-worker.js");
  }
  localStorage.setItem("cellar.alerts", "on");
  renderAlertButton();
  showToast("Browser alerts enabled for server events.");
}

function notifyOperator(title, body) {
  if (!alertsEnabled() || Notification.permission !== "granted") return;
  if (serviceWorker) {
    serviceWorker.showNotification(title, { body, tag: "cellar-server" });
  } else {
    new Notification(title, { body });
  }
}

/* ---- tabs --------------------------------------------------------------- */

function showTab(name) {
  activeTab = name;
  document.querySelectorAll("nav.tabs button").forEach((button) => {
    button.setAttribute("aria-selected", String(button.dataset.tab === name));
  });
  document.querySelectorAll("main section").forEach((section) => {
    section.hidden = section.id !== `tab-${name}`;
  });

  if (name === "records") loadDocuments();
  if (name === "database") loadDatabase();
  if (name === "players") loadPlayers();
  if (name === "access") loadAccess();
  if (name === "releases") loadReleases();
  if (name === "settings") loadSettings();
  if (name === "monitoring") refreshStatus();
  if (name === "configs") loadConfigs();
}

/* ---- access ------------------------------------------------------------- */

async function loadAccess() {
  const response = await fetch("/api/access");
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
  const response = await fetch("/api/access", {
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

  const response = await fetch("/api/settings");
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
  const response = await fetch("/api/settings", {
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
  const response = await fetch(`/api/settings/export?format=${format}&overrides=${overrides}`);
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
    const response = await fetch("/api/status");
    if (response.status === 401) return showGate();
    data = await response.json();
  } catch {
    setLamp($("#stat-state"), "down", "unreachable");
    return;
  }

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
      crash_looping: "down",
      stopped: "down",
    };
    setLamp($("#stat-state"), lamps[server.state] || "wait", server.state.replace("_", " "));

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
  applyTableTools();
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

function applyTableTools() {
  for (const [inputId, bodyId, sortId] of tableTools) {
    const input = $("#" + inputId);
    const body = $("#" + bodyId);
    if (!input || !body) continue;
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

function setLamp(node, state, label) {
  node.className = `value lamp ${state}`;
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

function appendLine(kind, at, who, message, live = false, level = "info", category = null) {
  consoleRecords.push({ kind, at, who, message, live, level, category: category || logCategory(who, message) });
  if (consoleRecords.length > 5000) consoleRecords.shift();
  if (consoleSlow && live) {
    if (!consoleRenderTimer) consoleRenderTimer = setTimeout(() => {
      consoleRenderTimer = null;
      renderConsole();
    }, 1000);
    return;
  }
  renderConsole();
}

function renderConsole() {
  const console_ = $("#console");
  const pinned = console_.scrollTop + console_.clientHeight >= console_.scrollHeight - 40;
  if (consolePaused) return;
  const query = text($("#console-filter")?.value).trim().toLowerCase();
  const minimum = $("#console-level")?.value || "";
  const category = $("#console-category")?.value || "";
  const view = $("#console-view")?.value || "all";
  const rank = { trace: 0, debug: 1, info: 2, warning: 3, error: 4 };
  const minimumRank = minimum ? rank[minimum] : -1;
  console_.replaceChildren();
  for (const record of consoleRecords) {
    const searchable = `${record.who} ${record.message}`.toLowerCase();
    if (query && !searchable.includes(query)) continue;
    if (minimumRank >= 0 && (rank[record.level] ?? 2) < minimumRank) continue;
    if (category && record.category !== category) continue;
    if (view === "command" && !["echo", "reply"].includes(record.kind)) continue;
    if (view === "background" && !["log", "join", "leave"].includes(record.kind)) continue;
    if (view === "errors" && record.level !== "error" && record.kind !== "error") continue;
    const levels = ["trace", "debug", "info", "warning", "error"];
    const categories = ["cellar", "engine", "gameplay", "network", "players", "storage", "other"];
    const level = levels.includes(record.level) ? record.level : "info";
    const logCategoryName = categories.includes(record.category) ? record.category : "other";
    const line = el("div", `line ${record.kind} level-${level} category-${logCategoryName}`);
    line.append(el("span", "at", record.at), el("span", "who", record.who), el("span", "msg", record.message));
    console_.append(line);
    while (console_.children.length > 1500) console_.firstChild.remove();
  }
  if (pinned) console_.scrollTop = console_.scrollHeight;
}

function logCategory(who, message) {
  const value = `${who} ${message}`.toLowerCase();
  if (/storage|database|document/.test(value)) return "storage";
  if (/network|connect|lobby/.test(value)) return "network";
  if (/player|identity|chat/.test(value)) return "players";
  if (/physics|render|map/.test(value)) return "engine";
  if (/applejack|game/.test(value)) return "gameplay";
  if (/cellar/.test(value)) return "cellar";
  return "other";
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

async function runCommand(command) {
  if (!command.trim()) return;
  appendLine("echo", now(), "you", `> ${command}`);

  try {
    const response = await fetch("/api/exec", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ command }),
    });
    const data = await response.json();

    if (!response.ok) {
      appendLine("error", now(), "cellar", text(data.error));
      return;
    }
    for (const line of data.reply) appendLine("reply", now(), "reply", text(line));
    if (!data.reply.length) appendLine("reply", now(), "reply", "(no output)");
  } catch (error) {
    appendLine("error", now(), "cellar", String(error));
  }
}

/* ---- live events -------------------------------------------------------- */

function connect() {
  const protocol = location.protocol === "https:" ? "wss" : "ws";
  socket = new WebSocket(`${protocol}://${location.host}/api/events`);

  socket.onopen = () => setLamp($("#connection-state"), "up", "live");
  socket.onerror = () => setLamp($("#connection-state"), "down", "error");

  socket.onmessage = (message) => {
    const event = JSON.parse(message.data);
    switch (event.kind) {
      case "log":
        appendLine(event.level === "error" ? "error" : "", clock(event.at), text(event.logger), text(event.message), true, event.level);
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
      case "process_exited":
      case "server_ready":
        appendLine("echo", now(), "cellar", event.kind.replace("_", " "));
        if (event.kind === "process_exited" && !event.graceful) {
          notifyOperator("Cellar server alert", "The server exited unexpectedly.");
        }
        if (event.kind === "server_ready") {
          notifyOperator("Cellar server ready", event.map ? `Ready on ${event.map}.` : "Ready for players.");
        }
        refreshStatus();
        break;
      default:
        break;
    }
  };

  // Reconnect rather than going quietly dead: a dashboard that stops updating
  // without saying so is worse than one that is obviously offline.
  socket.onclose = () => {
    setLamp($("#connection-state"), "down", "reconnecting");
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
  const response = await fetch(`/api/docs?prefix=${encodeURIComponent(prefix)}`);
  const documents = await response.json();

  if (!Array.isArray(documents)) {
    const row = el("tr");
    const cell = el("td", "muted", text(documents.error));
    cell.colSpan = 4;
    row.append(cell);
    body.append(row);
    return;
  }

  for (const document_ of documents) {
    const row = el("tr");
    const open = el("button", "chip", "open");
    open.onclick = () => openDocument(document_.key);

    const actions = el("td");
    actions.append(open);

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
  const response = await fetch(`/api/docs/${key}`);
  const data = await response.json();

  $("#doc-title").textContent = key;
  $("#doc-body").textContent = JSON.stringify(data.document.body, null, 2);

  const history = $("#doc-history");
  history.replaceChildren();
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
  if (!Array.isArray(tables)) return;

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

function start() {
  if (started) return;
  started = true;
  loadLogs();
  connect();
  refreshStatus();
  loadReleases();
  refreshBuildHealth();
  setInterval(() => {
    refreshStatus();
    if (activeTab === "database") loadDatabase();
  }, 2000);
  setInterval(refreshBuildHealth, 30000);
  renderAlertButton();
  if (alertsEnabled() && "serviceWorker" in navigator) {
    navigator.serviceWorker.register("/service-worker.js").then((registration) => {
      serviceWorker = registration;
    }).catch(() => showToast("Alerts could not be restored."));
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
  const response = await fetch("/api/logs?limit=250");
  if (!response.ok) return;
  const data = await response.json();
  for (const line of data.lines || []) appendLine(line.level === "error" ? "error" : "", clock(line.at), line.tag, line.message, false, line.level, line.category);
  $("#console-state").textContent = `${data.lines?.length || 0} recent lines · ${data.scanned_files || 0} persistent log file(s)`;
}

async function importSettings(apply) {
  const file = $("#settings-import-file").files?.[0];
  if (!file) {
    $("#settings-import-notice").textContent = "Choose a TOML or YAML settings file first.";
    return;
  }
  if (apply && !confirm(`Apply settings from ${file.name}?`)) return;
  const response = await fetch("/api/settings/import", {
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
  if (level) params.set("level", level);
  if (category) params.set("category", category);
  const response = await fetch(`/api/logs?${params}`);
  const data = await response.json();
  if (!response.ok) return showToast(text(data.error));
  consoleRecords = [];
  for (const line of data.lines || []) appendLine(line.level === "error" ? "error" : "", clock(line.at), line.tag, line.message, false, line.level, line.category);
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
  const modeActions = $("#config-mode-actions");
  modeActions.replaceChildren();
  for (const mode of ["development", "published"]) {
    const profile = profiles.find((candidate) => candidate.mode === mode);
    const label = mode === "development" ? "Use Development mode" : "Use Published mode";
    const button = el("button", `action ${profile?.active ? "live" : ""}`, profile ? label : `${label} unavailable`);
    button.disabled = !profile || profile.active;
    button.title = profile ? `Switch to ${profile.name}` : "Install or copy the matching AppleJackRP profile beside the active config";
    if (profile) button.onclick = () => activateConfig(profile.name);
    modeActions.append(button);
  }
  for (const profile of profiles) {
    const mode = profile.mode === "published" ? "Published" : "Development";
    const targetName = profile.game || profile.project || "local project";
    const button = el("button", `chip ${profile.active ? "live" : ""}`, `${mode} · ${profile.name} · ${targetName}`);
    button.disabled = profile.active;
    button.onclick = () => activateConfig(profile.name);
    target.append(button);
  }
}

async function activateConfig(name) {
  if (!confirm(`Switch to ${name}? The supervised server will restart.`)) return;
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
    button.onclick = () => showTab(button.dataset.tab);
  });

  $("#login").onsubmit = signIn;

  $("#command").addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      runCommand($("#command").value);
      $("#command").value = "";
    }
  });

  document.querySelectorAll(".chip[data-command]").forEach((chip) => {
    chip.onclick = () => runCommand(chip.dataset.command);
  });

  $("#doc-search").onclick = loadDocuments;
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
  $("#console-slow").onclick = () => {
    consoleSlow = !consoleSlow;
    $("#console-slow").textContent = consoleSlow ? "normal mode" : "slow mode";
    renderConsole();
  };
  $("#console-scan").onclick = scanLogs;
  $("#console-clear").onclick = () => { consoleRecords = []; renderConsole(); };
  $("#console-filter").value = localStorage.getItem("cellar.console.filter") || "";
  $("#console-level").value = localStorage.getItem("cellar.console.level") || "";
  $("#console-view").value = localStorage.getItem("cellar.console.view") || "all";
  $("#settings-import-preview").onclick = () => importSettings(false);
  $("#settings-import-apply").onclick = () => importSettings(true);
  for (const [inputId, , sortId] of tableTools) {
    $("#" + inputId)?.addEventListener("input", applyTableTools);
    $("#" + sortId)?.addEventListener("change", applyTableTools);
  }

  const probe = await fetch("/api/status");
  if (probe.status === 401) {
    showGate();
  } else {
    $("#app").hidden = false;
    start();
  }
});

async function control(action) {
  if (!confirm(`Really ${action} the server?`)) return;
  await fetch(`/api/control/${action}`, { method: "POST" });
  refreshStatus();
}
