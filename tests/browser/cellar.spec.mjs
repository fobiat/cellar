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
  const common = (id, name, log, args, profileName, prefix) => `
[instances.${id}]
scope = ${tomlString(id)}

[instances.${id}.server]
executable = ${tomlString(fake)}
project = ${tomlString(join(directory, `${id}.sbproj`))}
launcher = "native"
hostname = ${tomlString(name)}
log_file = ${tomlString(log)}
port = ${id === "alpha" ? 27115 : 27117}
query_port = ${id === "alpha" ? 27116 : 27118}
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
${common("alpha", "Alpha Sandbox", alphaLog, ["--players", "1"], "Alpha mode", "alpha")}
${common("beta", "Beta World", betaLog, ["--players", "2", "--flood"], "Beta mode", "beta")}

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

  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  expect(overflow).toBe(false);
  const results = await new AxeBuilder({ page }).analyze();
  expect(results.violations.filter((violation) => ["serious", "critical"].includes(violation.impact))).toEqual([]);
});

test("keeps destructive actions behind an explicit dialog", async ({ page }) => {
  await page.goto("/#/settings");
  await page.locator("#kill-cellar").click();
  await expect(page.locator("#confirm-dialog")).toBeVisible();
  await expect(page.locator("#confirm-body")).toContainText("terminated");
  await expect(page.locator("#confirm-go")).toBeDisabled();
  await page.locator("#confirm-cancel").click();
  await expect(page.locator("#confirm-dialog")).toBeHidden();
});
