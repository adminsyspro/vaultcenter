import { readFileSync } from "node:fs";
import http from "node:http";
import https from "node:https";
import { parseArgs } from "node:util";
import { fileURLToPath } from "node:url";
import next from "next";
import { createDemoHandler, assertLoopback } from "./dev-demo.mjs";

const { values } = parseArgs({
  options: {
    hostname: { type: "string", default: "127.0.0.1" },
    port: { type: "string", default: process.env.PORT || "3000" },
  },
});
const port = Number(values.port);
const demoMode = process.env.VAULTCENTER_DEV_DEMO === "1";
if (demoMode) assertLoopback(values.hostname);
const demoHandler = demoMode ? createDemoHandler({
  crowded: true, // enough tapes for the S3 queue and the finished list to overflow
  historyFile: fileURLToPath(new URL("../.local/demo-throughput-history.json", import.meta.url)),
  retentionDays: Number(process.env.VAULTCENTER_DEMO_HISTORY_RETENTION_DAYS || "30"),
}) : null;
// Without COLD_VTL_DEV_API_URL, proxy to a daemon on its default local listener.
const defaultUpstream = "https://127.0.0.1:8443";
let upstream = null;
if (!demoMode) {
  const configured = process.env.COLD_VTL_DEV_API_URL || defaultUpstream;
  try {
    upstream = new URL(configured);
  } catch {
    throw new Error(`COLD_VTL_DEV_API_URL is not a valid URL: ${configured}`);
  }
}
if (upstream && (
  !["http:", "https:"].includes(upstream.protocol) ||
  upstream.username || upstream.password || upstream.pathname !== "/" ||
  upstream.search || upstream.hash
)) {
  throw new Error("COLD_VTL_DEV_API_URL must be an HTTP(S) origin, e.g. https://vaultcenter.example.net:8443");
}
const transport = upstream?.protocol === "https:" ? https : http;
// COLD_VTL_DEV_CA trusts a private CA; otherwise Node's default trust store applies.
const caPath = process.env.COLD_VTL_DEV_CA;
let ca;
if (upstream?.protocol === "https:" && caPath) {
  try {
    ca = readFileSync(caPath);
  } catch (error) {
    throw new Error(`Cannot read the CA certificate in COLD_VTL_DEV_CA (${caPath}): ${error.message}`);
  }
}
const cert = process.env.COLD_VTL_DEV_TLS_CERT;
const key = process.env.COLD_VTL_DEV_TLS_KEY;
if (Boolean(cert) !== Boolean(key)) {
  throw new Error("Configure both COLD_VTL_DEV_TLS_CERT and COLD_VTL_DEV_TLS_KEY");
}
const scheme = cert ? "https" : "http";
const localCookie = "cold_vtl_dev_proxy";
const upstreamCookie = "__Host-cold_vtl";

function proxy(req, res) {
  // Browsers may have cached Next's former trailing-slash redirects.
  const path = new URL(req.url, "http://localhost");
  path.pathname = path.pathname.replace(/\/$/, "");
  const origin = req.headers.origin;
  if (
    (!cert && !/^(localhost|127\.0\.0\.1|\[::1\])(?::\d+)?$/.test(req.headers.host || "")) ||
    (origin && origin !== `${scheme}://${req.headers.host}`) ||
    req.headers["sec-fetch-site"] === "cross-site"
  ) {
    res.writeHead(403, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: "Cross-origin request rejected" }));
    return;
  }
  // OIDC uses the callback registered for the appliance. Run that flow there.
  if (path.pathname === "/auth/oidc/login") {
    res.writeHead(302, { Location: `${upstream.origin}/auth/oidc/login` });
    res.end();
    return;
  }
  const headers = { ...req.headers, host: upstream.host };
  if (origin) headers.origin = upstream.origin;
  // HTTP is allowed only on localhost; the backend still receives its HTTPS cookie.
  if (!cert && headers.cookie) {
    headers.cookie = headers.cookie.split(/;\s*/)
      .filter(cookie => !cookie.startsWith(`${upstreamCookie}=`))
      .map(cookie => cookie.startsWith(`${localCookie}=`)
        ? `${upstreamCookie}=${cookie.slice(localCookie.length + 1)}` : cookie)
      .join("; ");
  }
  const pending = transport.request(upstream, {
    method: req.method,
    path: path.pathname + path.search,
    headers,
    ca,
    timeout: 30_000,
  }, (response) => {
    const responseHeaders = { ...response.headers };
    if (!cert && responseHeaders["set-cookie"]) {
      responseHeaders["set-cookie"] = responseHeaders["set-cookie"].map(cookie =>
        cookie.startsWith(`${upstreamCookie}=`)
          ? `${localCookie}=${cookie.slice(upstreamCookie.length + 1)}`.replace(/;\s*Secure\b/gi, "")
          : cookie,
      );
    }
    res.writeHead(response.statusCode, responseHeaders);
    response.pipe(res);
    response.on("error", () => res.destroy());
  });
  pending.on("timeout", () => pending.destroy(new Error("API timeout")));
  pending.on("error", (error) => {
    console.error(`Development API proxy: ${error.message}`);
    if (res.headersSent) return res.destroy();
    res.writeHead(502, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: `Development API unreachable at ${upstream.origin}` }));
  });
  req.on("aborted", () => pending.destroy());
  req.pipe(pending);
}

const listener = (req, res) => {
  if (demoHandler?.(req, res)) return;
  if (/^\/(?:auth\/|v1\/|health(?:\?|$))/.test(req.url)) {
    if (demoMode) { res.writeHead(404); res.end(); return; }
    proxy(req, res);
  } else {
    handle(req, res);
  }
};
const server = cert
  ? https.createServer({ cert: readFileSync(cert), key: readFileSync(key) }, listener)
  : http.createServer(listener);
server.on("close", () => demoHandler?.close());
const app = next({ dev: true, hostname: values.hostname, port, httpServer: server });
const handle = app.getRequestHandler();
await app.prepare();
server.listen(port, values.hostname, () => {
  console.log(`VaultCenter dev: ${scheme}://localhost:${port} → ${demoMode ? "local demo (no proxy)" : upstream.origin}`);
});
