import { readFile, mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn } from "node:child_process";

import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

const root = join(import.meta.dirname, "..", "..");
const cellar = join(root, "target", "debug", "cellar");
const fake = join(root, "target", "debug", "cellar-fake-server");

let state;

function tomlString(value) {
  return JSON.stringify(value);
}

async function writeFixture(port, { auth = "none", twoInstances = true } = {}) {
  const directory = await mkdtemp(join(tmpdir(), "cellar-browser-"));
  const alphaLog = join(directory, "alpha.log");
  const betaLog = join(directory, "beta.log");
  const gamePort = 28000 + (port % 1000) * 2;
  const common = (id, name, log, args, profileName, prefix, offset) => `
[instances.${id}]
scope = ${tomlString(id)}

[instances.${id}.server]
executable = ${tomlString(fake)}
project = ${tomlString(join(directory, `${id}.sbproj`))}
launcher = "native"
hostname = ${tomlString(name)}
log_file = ${tomlString(log)}
port = ${gamePort + offset}
query_port = ${gamePort + offset + 1}
ready_pattern = "Server is ready"
extra_args = ${JSON.stringify(["--log-file", log, "--hostname", name, ...args])}

[instances.${id}.profile]
name = ${tomlString(profileName)}
ready_pattern = "Server is ready"
convar_prefix = ${tomlString(prefix)}

[[instances.${id}.profile.command]]
group = "general"
label = "Status"
command = "status"
`;
  const config = `
${common("alpha", "Alpha Sandbox", alphaLog, ["--players", "1"], "Alpha mode", "alpha", 0)}
${twoInstances ? common("beta", "Beta World", betaLog, ["--players", "2"], "Beta mode", "beta", 2) : ""}

[web]
enabled = true
bind = "127.0.0.1:${port}"
auth = ${tomlString(auth)}
`;
  const configPath = join(directory, "cellar.toml");
  await writeFile(configPath, config);
  return { directory, configPath };
}

async function waitFor(url) {
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`Cellar did not answer ${url}`);
}

function startCellar(configPath) {
  const child = spawn(cellar, ["--config", configPath, "run"], {
    cwd: root,
    env: { ...process.env, CELLAR_LOG: "warn" },
    stdio: "ignore",
  });
  child.on("error", (error) => { throw error; });
  return child;
}

test.beforeAll(async ({}, testInfo) => {
  const port = Number(new URL(testInfo.project.use.baseURL).port);
  state = await writeFixture(port);
  state.child = startCellar(state.configPath);
  await waitFor(`http://127.0.0.1:${port}/healthz`);
  await waitFor(`http://127.0.0.1:${port}/api/status`);
  const authPort = port + 100;
  state.auth = await writeFixture(authPort, { auth: "password", twoInstances: false });
  state.auth.port = authPort;
  state.auth.child = startCellar(state.auth.configPath);
  await waitFor(`http://127.0.0.1:${authPort}/healthz`);
});

test.afterAll(async () => {
  for (const fixture of [state, state?.auth]) {
    if (fixture?.child && fixture.child.exitCode === null) {
      fixture.child.kill("SIGINT");
      await new Promise((resolve) => fixture.child.once("exit", resolve));
    }
    if (fixture?.directory) await rm(fixture.directory, { recursive: true, force: true });
  }
});

async function expectAccessible(page) {
  const results = await new AxeBuilder({ page }).exclude("#console").analyze();
  expect(results.violations.filter((violation) => ["serious", "critical"].includes(violation.impact))).toEqual([]);
}

async function section(page, name) {
  if (await page.locator("#section-select").isVisible()) await page.locator("#section-select").selectOption(name);
  else await page.locator(`#tabfor-${name}`).click();
}

test("covers the two instances, keyboard tabs, themes, mobile shell, and accessibility", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("#app")).toBeVisible();
  await expect(page.locator("#instance-strip .instance")).toHaveCount(2);
  await expect(page.locator("#identity-summary")).toContainText("Alpha Sandbox");

  if (await page.locator("#section-select").isVisible()) {
    await page.locator("#section-select").focus();
    await page.locator("#section-select").selectOption("settings");
  } else {
    await page.locator("#tabfor-settings").focus();
    await page.keyboard.press("Enter");
  }
  await expect(page.locator("#tab-settings")).toBeVisible();
  await page.locator("#theme").selectOption("light");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.locator("#theme").selectOption("dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  await page.locator("#instance-strip .instance").nth(1).click();
  await expect(page.locator("#identity-summary")).toContainText("Beta World");
  await section(page, "dispatch");
  await expect(page.locator("#precinct-title")).toContainText("Beta mode commands");
  await expect(page.locator("#precinct-title")).not.toContainText("AppleJack");
  await expect(page.locator("#precinct-palette")).not.toContainText(/applejack/i);

  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  expect(overflow).toBe(false);
  const strip = await page.locator(".strip").evaluate((element) => ({
    clientWidth: element.clientWidth,
    scrollWidth: element.scrollWidth,
  }));
  expect(strip.scrollWidth).toBeLessThanOrEqual(strip.clientWidth + 1);
  const tabs = await page.locator("nav.tabs").evaluate((element) => ({
    clientWidth: element.clientWidth,
    scrollWidth: element.scrollWidth,
  }));
  expect(tabs.scrollWidth).toBeLessThanOrEqual(tabs.clientWidth + 1);
  await expectAccessible(page);
});

test("covers first-run password setup and authenticated login accessibly", async ({ page }) => {
  const base = `http://127.0.0.1:${state.auth.port}`;
  const password = "correct horse battery staple";
  await page.goto(base);
  await expect(page.locator("#gate")).toBeVisible();
  await expect(page.locator("#login-copy")).toContainText("Choose the operator password");
  await expectAccessible(page);

  await page.locator("#password").fill(password);
  await page.locator("#password-confirm").fill(password);
  await page.locator("#login-submit").click();
  await expect(page.locator("#app")).toBeVisible({ timeout: 20_000 });

  await page.locator("#logout").click();
  await expect(page.locator("#gate")).toBeVisible();
  await expect(page.locator("#login-copy")).toContainText("Operator sign in");
  await page.locator("#password").fill("incorrect password value");
  await page.locator("#login-submit").click();
  await expect(page.locator("#gate-notice")).toContainText("not accepted", { timeout: 20_000 });
  await expectAccessible(page);

  await page.locator("#password").fill(password);
  await page.locator("#login-submit").click();
  await expect(page.locator("#app")).toBeVisible({ timeout: 20_000 });
});

test("customizes the overview canvas and persists the layout", async ({ page }) => {
  await page.goto("/#/overview");
  await page.evaluate(() => localStorage.removeItem("cellar.overview.layout"));
  await page.reload();
  await expect(page.locator("#overview-layout .overview-layout-row")).toHaveCount(22);
  await page.locator("#overview-customize").click();
  await expect(page.locator("#overview-layout")).toBeVisible();
  await expect(page.locator("#overview-layout")).toContainText("Game documents");
  await expect(page.locator("#overview-layout")).toContainText("Web access");

  const diagnostics = page.locator("#overview-layout .overview-layout-row", { hasText: "Diagnostics" });
  await diagnostics.locator("input").check();
  await expect(page.locator('[data-overview-id="diagnostics"]')).toBeVisible();

  if ((page.viewportSize()?.width || 0) > 1100) {
    const healthResize = page.locator('[data-overview-id="health"] .overview-resize-handle');
    await healthResize.scrollIntoViewIfNeeded();
    const box = await healthResize.boundingBox();
    if (!box) throw new Error("overview resize handle was not laid out");
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + 160, box.y + box.height / 2);
    await page.mouse.up();
    const resizedSpan = Number(await page.locator('[data-overview-id="health"]').getAttribute("data-overview-span"));
    expect(resizedSpan).toBeGreaterThan(4);

    await page.locator('[data-overview-id="activity"]').dragTo(
      page.locator('[data-overview-id="health"]'),
      { targetPosition: { x: 10, y: 10 } },
    );
    const order = await page.locator("#overview-grid .overview-card").evaluateAll((cards) => cards.map((card) => card.dataset.overviewId));
    expect(order.indexOf("activity")).toBeLessThan(order.indexOf("health"));
  }

  await page.reload();
  await expect(page.locator('[data-overview-id="diagnostics"]')).toBeVisible();
  await page.locator("#overview-customize").click();
  await expect(page.locator('#overview-layout .overview-layout-row', { hasText: "Diagnostics" }).locator("input")).toBeChecked();
  if ((page.viewportSize()?.width || 0) > 1100) {
    await expect(page.locator('[data-overview-id="health"]')).not.toHaveAttribute("data-overview-span", "4");
  }
  await page.locator("#overview-reset").click();
  await expect(page.locator('[data-overview-id="diagnostics"]')).toHaveCount(0);
  await expect(page.locator('[data-overview-id="health"]')).toHaveAttribute("data-overview-span", "4");
  await page.locator("#overview-customize").click();
  await expect(page.locator("#overview-layout")).toBeHidden();
  await expect(page.locator("#overview-customize")).toHaveText("Edit layout");
});

test("respects the console auto-scroll toggle", async ({ page }) => {
  await page.goto("/#/dispatch");
  const console = page.locator("#console");
  await expect(page.locator("#connection-state")).toHaveText("live");
  for (let index = 0; index < 24; index += 1) {
    await page.locator("#command").fill(`scroll-test-${index}`);
    await page.locator("#command").press("Enter");
  }
  await expect.poll(() => console.evaluate((element) => element.scrollHeight > element.clientHeight)).toBe(true);

  const toggle = page.locator("#console-autoscroll");
  await toggle.click();
  await expect(toggle).toHaveText("auto-scroll off");
  const before = await console.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
    return element.scrollTop;
  });
  const lines = await console.locator(".line").count();
  await page.locator("#command").fill("status");
  await page.locator("#command").press("Enter");
  await expect.poll(() => console.locator(".line").count()).toBeGreaterThan(lines);
  await expect.poll(() => console.evaluate((element) => element.scrollTop)).toBeLessThanOrEqual(before + 1);

  await toggle.click();
  await expect(toggle).toHaveText("auto-scroll on");
  const onLines = await console.locator(".line").count();
  await page.locator("#command").fill("status");
  await page.locator("#command").press("Enter");
  await expect.poll(() => console.locator(".line").count()).toBeGreaterThan(onLines);
  await expect.poll(() => console.evaluate((element) => element.scrollTop + element.clientHeight >= element.scrollHeight - 1)).toBe(true);
});

test("keeps a 50000-event burst bounded and leaves command entry responsive", async ({ page }) => {
  await page.goto("/#/dispatch");
  await expect(page.locator("#connection-state")).toHaveText("live");
  await expect(page.locator("#command")).toBeEnabled();
  const result = await page.evaluate(async () => {
    const started = performance.now();
    for (let index = 0; index < 50_000; index++) {
      socket.onmessage({ data: JSON.stringify({ kind: "log", at: new Date().toISOString(), logger: "burst", message: `Burst ${index}`, level: "info", category: "engine" }) });
    }
    await new Promise(requestAnimationFrame);
    return { elapsed: performance.now() - started, records: consoleRecords.length, rows: document.querySelector("#console").children.length };
  });
  expect(result.records).toBeLessThanOrEqual(5000);
  expect(result.rows).toBeLessThanOrEqual(1500);
  expect(result.elapsed).toBeLessThan(5000);
  await expect(page.locator("#console")).toContainText("Burst 49999");
  await page.locator("#command").fill("status");
  await page.locator("#command-send").click();
  await expect(page.locator("#command-feedback")).toContainText("Reply received");
});

test("keeps destructive actions behind an explicit dialog", async ({ page }) => {
  await page.goto("/#/settings");
  await expect(page.locator("#tab-settings")).toBeVisible();
  await page.locator("#kill-cellar").click();
  await expect(page.locator("#confirm-dialog")).toBeVisible();
  await expect(page.locator("#confirm-body")).toContainText("terminated");
  await expect(page.locator("#confirm-go")).toBeDisabled();
  await expect(page.locator("#confirm-typed")).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(page.locator("#confirm-cancel")).toBeFocused();
  await page.keyboard.press("Shift+Tab");
  await expect(page.locator("#confirm-typed")).toBeFocused();
  await page.locator("#confirm-typed").fill("KILL ALL");
  await expect(page.locator("#confirm-go")).toBeEnabled();
  await page.keyboard.press("Tab");
  await expect(page.locator("#confirm-cancel")).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(page.locator("#confirm-go")).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(page.locator("#confirm-typed")).toBeFocused();
  await expectAccessible(page);
  await page.keyboard.press("Escape");
  await expect(page.locator("#confirm-dialog")).toBeHidden();
  await expect(page.locator("#kill-cellar")).toBeFocused();
});

for (const route of ["overview", "dispatch", "monitoring", "players/connected", "players/history", "players/access", "database", "diagnostics", "activity", "records", "config/profile", "config/convars", "config/build", "settings"]) {
test(`covers ${route} in both themes without overflow or inaccessible controls`, async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("#workspace-state")).toContainText("running");
  for (const theme of ["dark", "light"]) {
    await page.locator("#theme").selectOption(theme);
      await page.evaluate(route => { location.hash = `#/i/alpha/${route}`; }, route);
      await expect(page.locator(`#tab-${route.split("/")[0]}`)).toBeVisible();
      await expect(page.locator("main [aria-busy=true]")).toHaveCount(0);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      await expectAccessible(page);
  }
});
}

test("supports 320px, 200% text, reduced motion and full status details", async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 900 });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/");
  await page.evaluate(() => { document.documentElement.style.fontSize = "200%"; });
  await expect(page.locator("#workspace-state")).toContainText("running");
  await page.locator(".status-details summary").click();
  await expect(page.locator("#stat-persistence")).toBeVisible();
  await expect(page.locator("#stat-state")).toContainText("running");
  for (const name of ["overview", "dispatch", "monitoring", "players", "database", "diagnostics", "activity", "records", "config", "settings"]) {
    await section(page, name);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  }
  await expectAccessible(page);
  const motion = await page.locator("#restart").evaluate(element => getComputedStyle(element).transitionDuration);
  expect(motion).toBe("0s");
});

test("keeps overview focus and provides keyboard layout controls and a modal jump palette", async ({ page }) => {
  await page.goto("/");
  const open = page.getByRole("button", { name: "Open Server health", exact: true });
  await open.focus();
  await page.evaluate(() => refreshStatus());
  await expect(open).toBeFocused();
  await page.locator("#overview-customize").click();
  const resize = page.getByRole("button", { name: "Resize Server health", exact: true });
  await resize.focus();
  await page.keyboard.press("ArrowRight");
  await expect(page.locator('[data-overview-id="health"]')).toHaveAttribute("data-overview-span", "5");
  await expect(resize).toBeFocused();
  await page.locator("#palette-open").click();
  await expect(page.locator("#palette-input")).toBeFocused();
  await page.locator("#palette-input").fill("no-match-here");
  await expect(page.locator("#palette-empty")).toBeVisible();
  await expectAccessible(page);
  await page.keyboard.press("Escape");
  await expect(page.locator("#palette-open")).toBeFocused();
  await page.locator("#palette-open").click();
  await page.locator("#palette-input").fill("Monitoring");
  await page.keyboard.press("Enter");
  await expect(page.locator("#tab-monitoring")).toBeVisible();
});

test("isolates delayed instance reads and reconnects the selected console stream", async ({ page }) => {
  let release;
  const held = new Promise(resolve => { release = resolve; });
  let requested;
  const pending = new Promise(resolve => { requested = resolve; });
  await page.goto("/#/i/alpha/records");
  await expect(page.locator("#workspace-state")).toContainText("running");
  await page.route("**/api/docs?**", async route => {
    if (new URL(route.request().url()).searchParams.get("instance") === "alpha") {
      requested();
      await held;
      await route.fulfill({ json: [{ key: "alpha-only", revision: 1, bytes: 10 }] });
    } else await route.fulfill({ json: [{ key: "beta-only", revision: 1, bytes: 10 }] });
  });
  await page.locator("#doc-search").click();
  await pending;
  await page.locator('[data-instance="beta"]').click();
  await expect(page.locator("#documents")).toContainText("beta-only");
  release();
  await expect(page.locator("#documents")).not.toContainText("alpha-only");
  await section(page, "dispatch");
  await expect(page.locator("#connection-state")).toHaveText("live");
  await page.locator("#command").fill("status");
  await page.locator("#command-send").click();
  await expect(page.locator("#command-feedback")).toContainText("Reply received from beta");
  await expect(page.locator("#console")).toContainText("Beta World");
  await expect(page.locator("#console")).not.toContainText("Alpha Sandbox");
});

test("explains lifecycle failures and refuses unconfirmed command success", async ({ page }) => {
  await page.goto("/#/dispatch");
  await expect(page.locator("#workspace-state")).toContainText("running");
  const snapshot = await (await page.request.get("/api/status?instance=alpha")).json();
  for (const [state, advice] of [["starting", "readiness signal"], ["unhealthy", "has not reached readiness"], ["crash_looping", "Repeated crashes"], ["stop_failed", "Stop was not confirmed"], ["stopped", "Restart"]]) {
    await page.route("**/api/status?**", route => route.fulfill({ json: { ...snapshot, server: { ...snapshot.server, state } } }));
    await page.evaluate(() => refreshStatus());
    await expect(page.locator("#live-notice")).toContainText(advice);
    await page.unroute("**/api/status?**");
  }
  await page.evaluate(() => refreshStatus());
  await page.route("**/api/exec?**", route => route.fulfill({ status: 409, json: { error: "maintenance in progress" } }));
  await page.locator("#command").fill("status");
  await page.locator("#command-send").click();
  await expect(page.locator("#command-feedback")).toContainText("Command refused");
  await expect(page.locator("#command-feedback")).toContainText("maintenance in progress");
  await expectAccessible(page);
});

test("renders a disconnected server with actionable accessible text", async ({ page }) => {
  await page.goto("/#/dispatch");
  await expect(page.locator("#connection-state")).toHaveText("live");
  state.child.kill("SIGTERM");
  await new Promise((resolve) => state.child.once("exit", resolve));

  await expect(page.locator("#connection-state")).toHaveText("reconnecting");
  await expect(page.locator("#console")).toContainText("disconnected: lines from here are recovered on reconnect");
  await expectAccessible(page);
});
