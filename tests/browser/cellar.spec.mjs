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

async function writeFixture(port) {
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
${common("beta", "Beta World", betaLog, ["--players", "2", "--flood"], "Beta mode", "beta", 2)}

[web]
enabled = true
bind = "127.0.0.1:${port}"
auth = "none"
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
});

test.afterAll(async () => {
  if (state?.child && state.child.exitCode === null) {
    state.child.kill("SIGINT");
    await new Promise((resolve) => state.child.once("exit", resolve));
  }
  if (state?.directory) await rm(state.directory, { recursive: true, force: true });
});

test("covers the two instances, keyboard tabs, themes, mobile shell, and accessibility", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("#app")).toBeVisible();
  await expect(page.locator("#instance-strip .instance")).toHaveCount(2);
  await expect(page.locator("#identity-summary")).toContainText("Alpha Sandbox");

  await page.locator("#tabfor-settings").focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("#tab-settings")).toBeVisible();
  await page.locator("#theme").selectOption("light");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.locator("#theme").selectOption("dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  await page.locator("#instance-strip .instance").nth(1).click();
  await expect(page.locator("#identity-summary")).toContainText("Beta World");
  await page.locator("#tabfor-dispatch").click();
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
  const results = await new AxeBuilder({ page }).analyze();
  expect(results.violations.filter((violation) => ["serious", "critical"].includes(violation.impact))).toEqual([]);
});

test("customizes the overview canvas and persists the layout", async ({ page }) => {
  await page.goto("/#/overview");
  await page.evaluate(() => localStorage.removeItem("cellar.overview.layout"));
  await page.reload();
  await page.locator("#overview-customize").click();
  await expect(page.locator("#overview-layout")).toBeVisible();
  await expect(page.locator("#overview-layout .overview-layout-row")).toHaveCount(22);
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

    await page.locator('[data-overview-id="activity"]').dragTo(page.locator('[data-overview-id="health"]'));
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

test("keeps destructive actions behind an explicit dialog", async ({ page }) => {
  await page.goto("/#/settings");
  await expect(page.locator("#tab-settings")).toBeVisible();
  // The mobile tab bar and disclosure panels can overlap the auto-scroll hit
  // area while the control is already visible. The assertion is about the
  // typed confirmation, so click the resolved control directly.
  await page.locator("#kill-cellar").click({ force: true });
  await expect(page.locator("#confirm-dialog")).toBeVisible();
  await expect(page.locator("#confirm-body")).toContainText("terminated");
  await expect(page.locator("#confirm-go")).toBeDisabled();
  await page.locator("#confirm-cancel").click();
  await expect(page.locator("#confirm-dialog")).toBeHidden();
});
