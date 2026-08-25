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

/* ---- tabs --------------------------------------------------------------- */

function showTab(name) {
  document.querySelectorAll("nav.tabs button").forEach((button) => {
    button.setAttribute("aria-selected", String(button.dataset.tab === name));
  });
  document.querySelectorAll("main section").forEach((section) => {
    section.hidden = section.id !== `tab-${name}`;
  });

  if (name === "records") loadDocuments();
  if (name === "database") loadTables();
  if (name === "players") loadPlayers();
  if (name === "releases") loadReleases();
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
      $("#stat-cpu").textContent = `${server.resources.cpu_percent.toFixed(0)}%`;
      cpuHistory.push(server.resources.cpu_percent);
      if (cpuHistory.length > 120) cpuHistory.shift();
      drawSpark($("#spark-cpu"), cpuHistory);
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

function appendLine(kind, at, who, message) {
  const console_ = $("#console");
  const pinned = console_.scrollTop + console_.clientHeight >= console_.scrollHeight - 40;

  const line = el("div", `line ${kind}`);
  line.append(el("span", "at", at), el("span", "who", who), el("span", "msg", message));
  console_.append(line);

  while (console_.children.length > 1500) console_.firstChild.remove();
  if (pinned) console_.scrollTop = console_.scrollHeight;
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
    for (const line of data.reply) appendLine("", now(), "reply", text(line));
    if (!data.reply.length) appendLine("", now(), "reply", "(no output)");
  } catch (error) {
    appendLine("error", now(), "cellar", String(error));
  }
}

/* ---- live events -------------------------------------------------------- */

function connect() {
  const protocol = location.protocol === "https:" ? "wss" : "ws";
  socket = new WebSocket(`${protocol}://${location.host}/api/events`);

  socket.onmessage = (message) => {
    const event = JSON.parse(message.data);
    switch (event.kind) {
      case "log":
        appendLine(event.level === "error" ? "error" : "", clock(event.at), text(event.logger), text(event.message));
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
        refreshStatus();
        break;
      default:
        break;
    }
  };

  // Reconnect rather than going quietly dead: a dashboard that stops updating
  // without saying so is worse than one that is obviously offline.
  socket.onclose = () => setTimeout(connect, 3000);
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

function formatBytes(bytes) {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = Number(bytes) || 0;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${value} B` : `${value.toFixed(1)} ${units[unit]}`;
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
  svg.replaceChildren();
  if (values.length < 2) return;

  const width = svg.clientWidth || 300;
  const height = 40;
  const peak = Math.max(100, ...values);

  const points = values
    .map((value, index) => {
      const x = (index / (values.length - 1)) * width;
      const y = height - (value / peak) * (height - 4) - 2;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");

  const line = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
  line.setAttribute("points", points);
  line.setAttribute("fill", "none");
  line.setAttribute("stroke", "currentColor");
  line.setAttribute("stroke-width", "1.5");
  svg.append(line);
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
  connect();
  refreshStatus();
  setInterval(refreshStatus, 2000);
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

  $("#stop").onclick = () => control("stop");
  $("#restart").onclick = () => control("restart");

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
