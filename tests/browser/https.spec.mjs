import { mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import { spawn, execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer } from "node:https";
import { request as httpRequest } from "node:http";
import { connect } from "node:net";
import { test, expect } from "@playwright/test";

test("authenticates over TLS with host-only Secure cookies and same-origin WSS", async ({ browser }, testInfo) => {
  const directory = await mkdtemp(join(tmpdir(), "cellar-https-"));
  const root = join(import.meta.dirname, "../..");
  const port = Number(new URL(testInfo.project.use.baseURL).port) + 200;
  const tlsPort = port + 100;
  const origin = `https://localhost:${tlsPort}`;
  const password = "local test operator password";
  const configPath = join(directory, "cellar.toml");
  const log = join(directory, "server.log");
  const config = `[server]\nexecutable = ${JSON.stringify(join(root, "target/debug/cellar-fake-server"))}\nproject = ${JSON.stringify(join(directory, "game.sbproj"))}\nlauncher = "native"\nlog_file = ${JSON.stringify(log)}\nready_pattern = "Server is ready"\nextra_args = ["--log-file", ${JSON.stringify(log)}]\n[web]\nenabled = true\nbind = "127.0.0.1:${port}"\nauth = "password"\n`;
  let child;
  let proxy;
  let context;
  const start = hash => spawn(join(root, "target/debug/cellar"), ["--config", configPath, "run"], {
    env: { ...process.env, ...(hash ? { CELLAR_WEB_PASSWORD_HASH: hash } : {}) }, stdio: "ignore",
  });
  const stop = async () => {
    if (child?.exitCode === null) {
      const done = new Promise(resolve => child.once("exit", resolve));
      child.kill("SIGINT"); await done;
    }
  };
  const wait = async expected => {
    await expect.poll(async () => {
      try { return (await fetch(`http://127.0.0.1:${port}/api/auth`)).status; } catch { return 0; }
    }).toBe(expected);
  };
  try {
    await writeFile(configPath, config);
    child = start();
    await wait(200);
    const setup = await fetch(`http://127.0.0.1:${port}/api/setup-password`, {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ password, confirmation: password }),
    });
    expect(setup.ok).toBe(true);
    await stop();
    const hash = (await readFile(join(directory, ".cellar.web-password"), "utf8")).trim();
    await writeFile(configPath, `${config}secure_cookies = true\npublic_origin = "${origin}"\n[web.tailscale]\nenabled = false\n`);
    child = start(hash);
    await wait(421);
    const key = join(directory, "key.pem");
    const cert = join(directory, "cert.pem");
    execFileSync("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost", "-keyout", key, "-out", cert], { stdio: "ignore" });
    proxy = createServer({ key: await readFile(key), cert: await readFile(cert), minVersion: "TLSv1.2" }, (incoming, outgoing) => {
      const upstream = httpRequest({ hostname: "127.0.0.1", port, path: incoming.url, method: incoming.method, headers: incoming.headers }, response => {
        outgoing.writeHead(response.statusCode, response.headers); response.pipe(outgoing);
      });
      upstream.on("error", () => { outgoing.writeHead(502); outgoing.end(); });
      incoming.pipe(upstream);
    });
    proxy.on("upgrade", (request, socket, head) => {
      const upstream = connect(port, "127.0.0.1", () => {
        upstream.write(`${request.method} ${request.url} HTTP/1.1\r\n${Object.entries(request.headers).map(([key, value]) => `${key}: ${value}`).join("\r\n")}\r\n\r\n`);
        if (head.length) upstream.write(head);
        socket.pipe(upstream).pipe(socket);
      });
      upstream.on("error", () => socket.destroy());
      socket.on("error", () => upstream.destroy());
      socket.on("close", () => upstream.destroy());
    });
    await new Promise(resolve => proxy.listen(tlsPort, "127.0.0.1", resolve));
    context = await browser.newContext({ ignoreHTTPSErrors: true });
    const page = await context.newPage();
    const response = await page.goto(origin);
    expect(response.headers()["strict-transport-security"]).toBe("max-age=31536000");
    expect(response.headers()["cache-control"]).toBe("no-store");
    await page.locator("#password").fill(password);
    await page.locator("#login-submit").click();
    await expect(page.locator("#app")).toBeVisible();
    await expect(page.locator("#connection-state")).toHaveText("live");
    const cookies = await context.cookies();
    const cookie = cookies.find(cookie => cookie.name === "__Host-cellar_session");
    expect(cookie).toMatchObject({ secure: true, httpOnly: true, sameSite: "Strict", path: "/" });
    expect(await page.evaluate(() => document.cookie)).not.toContain("cellar_session");
    const refused = await context.request.post(`${origin}/api/logout`, { headers: { origin: `http://localhost:${tlsPort}` } });
    expect(refused.status()).toBe(403);
    const forwarded = await fetch(`http://127.0.0.1:${port}/api/auth`, { headers: { "x-forwarded-host": `localhost:${tlsPort}`, "x-forwarded-proto": "https" } });
    expect(forwarded.status).toBe(421);
    await page.locator("#logout").click();
    await expect(page.locator("#gate")).toBeVisible();
  } finally {
    await context?.close();
    if (proxy) await new Promise(resolve => proxy.close(resolve));
    await stop();
    await rm(directory, { recursive: true, force: true });
  }
});
