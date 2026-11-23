let session = null;
const stateLabels = {
  writable: "Inscriptible",
  sealed: "Scellée",
  uploading: "Export en cours",
  verified: "Vérifiée",
  archiving: "Archivage",
  archived: "Archivée",
  restoring: "Restauration",
  hydrating: "Hydratation",
  online: "En ligne",
  failed: "Échec",
};

const $ = (selector) => document.querySelector(selector);
let toastTimer;

function showToast(message, error = false) {
  const toast = $("#toast");
  toast.textContent = message;
  toast.className = `toast visible${error ? " error" : ""}`;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { toast.className = "toast"; }, 4200);
}

async function api(path, options = {}) {
  const response = await fetch(path, {
    ...options,
    headers: { "content-type": "application/json", ...(session?.csrf_token ? { "X-CSRF-Token": session.csrf_token } : {}), ...(options.headers || {}) },
  });
  const body = await response.json().catch(() => ({}));
  if (response.status === 401 && path !== "/auth/login") signedOut();
  if (!response.ok) throw new Error(body.error || `Erreur HTTP ${response.status}`);
  return body;
}

function formatBytes(value) {
  if (!Number.isFinite(value) || value === 0) return "0 o";
  const units = ["o", "Kio", "Mio", "Gio", "Tio"];
  const rank = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
  return `${(value / (1024 ** rank)).toFixed(rank > 1 ? 2 : 0)} ${units[rank]}`;
}

function cell(text, className = "") {
  const node = document.createElement("td");
  node.textContent = text;
  if (className) node.className = className;
  return node;
}

function actionButton(label, className, disabled, handler) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = `action ${className}`;
  button.textContent = label;
  button.disabled = disabled || session?.user.role !== "admin";
  button.addEventListener("click", handler);
  return button;
}

function renderCartridges(cartridges) {
  const tbody = $("#cartridge-rows");
  tbody.replaceChildren();
  for (const cartridge of cartridges) {
    const row = document.createElement("tr");
    row.append(cell(cartridge.barcode, "barcode"));

    const stateCell = document.createElement("td");
    const badge = document.createElement("span");
    badge.className = `badge ${cartridge.state}`;
    badge.textContent = stateLabels[cartridge.state] || cartridge.state;
    stateCell.append(badge);
    row.append(stateCell);

    const usageCell = document.createElement("td");
    usageCell.className = "usage";
    const ratio = Math.min(100, (cartridge.bytes_written / cartridge.capacity_bytes) * 100);
    const meter = document.createElement("div");
    meter.className = "meter";
    const fill = document.createElement("span");
    fill.style.width = `${ratio}%`;
    meter.append(fill);
    const usageText = document.createElement("small");
    usageText.textContent = `${formatBytes(cartridge.bytes_written)} / ${formatBytes(cartridge.capacity_bytes)}`;
    usageCell.append(meter, usageText);
    row.append(usageCell);
    row.append(cell(String(cartridge.generation)));

    const actions = document.createElement("td");
    actions.className = "actions";
    actions.append(
      actionButton("Sceller", "", cartridge.state !== "writable", () => seal(cartridge.barcode)),
      actionButton("Exporter", "primary", cartridge.state !== "sealed", () => startExport(cartridge.barcode)),
    );
    row.append(actions);
    tbody.append(row);
  }
  if (!cartridges.length) {
    const row = document.createElement("tr");
    const empty = cell("Aucune cartouche découverte.", "empty");
    empty.colSpan = 5;
    row.append(empty);
    tbody.append(row);
  }
}

function renderJobs(jobs) {
  const list = $("#job-list");
  list.replaceChildren();
  if (!jobs.length) {
    const empty = document.createElement("p");
    empty.className = "empty";
    empty.textContent = "Aucun job.";
    list.append(empty);
    return;
  }
  for (const job of jobs) {
    const node = document.createElement("article");
    node.className = "job";
    const title = document.createElement("div");
    title.className = "job-title";
    const strong = document.createElement("strong");
    strong.textContent = `${job.kind.toUpperCase()} · ${job.subject}`;
    const small = document.createElement("small");
    small.textContent = job.id.slice(0, 13);
    title.append(strong, small);
    const middle = document.createElement("div");
    const progress = document.createElement("div");
    progress.className = "job-progress";
    const fill = document.createElement("span");
    fill.style.width = `${job.progress}%`;
    progress.append(fill);
    const message = document.createElement("div");
    message.className = "job-message";
    message.textContent = job.message || "En attente";
    middle.append(progress, message);
    const jobState = document.createElement("div");
    jobState.className = "job-state";
    jobState.textContent = job.status;
    node.append(title, middle, jobState);
    list.append(node);
  }
}

async function refresh(silent = true) {
  if (!session) return;
  try {
    const [status, cartridges, jobs] = await Promise.all([
      api("/v1/status"), api("/v1/cartridges"), api("/v1/jobs"),
    ]);
    $("#node-status").textContent = "Système opérationnel";
    $("#cartridge-count").textContent = status.cartridges;
    $("#sealed-count").textContent = cartridges.filter((item) => item.state === "sealed").length;
    $("#verified-count").textContent = cartridges.filter((item) => item.state === "verified").length;
    $("#active-job-count").textContent = jobs.filter((item) => ["queued", "running"].includes(item.status)).length;
    renderCartridges(cartridges);
    renderJobs(jobs);
    if (!silent) showToast("Données actualisées");
  } catch (error) {
    $("#node-status").textContent = "API indisponible";
    if (!silent) showToast(error.message, true);
  }
}

async function seal(barcode) {
  try {
    await api(`/v1/cartridges/${encodeURIComponent(barcode)}/seal`, { method: "POST", body: "{}" });
    showToast(`${barcode} est scellée`);
    await refresh();
  } catch (error) { showToast(error.message, true); }
}

async function startExport(barcode) {
  try {
    const job = await api(`/v1/cartridges/${encodeURIComponent(barcode)}/export`, { method: "POST", body: "{}" });
    showToast(`Export lancé · ${job.id.slice(0, 8)}`);
    await refresh();
  } catch (error) { showToast(error.message, true); }
}

$("#refresh-button").addEventListener("click", () => refresh(false));
function signedOut() {
  session = null;
  $("#console-panel").style.display = "none";
  $("#login-panel").hidden = false;
  $("#cartridge-rows").replaceChildren();
}
function signedIn(value) {
  session = value;
  $("#login-panel").hidden = true;
  $("#console-panel").style.display = "";
  refresh(false);
}
$("#login-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = event.currentTarget;
  const fields = new FormData(form);
  try { signedIn(await api("/auth/login", { method:"POST", body:JSON.stringify({ username:fields.get("username"),password:fields.get("password") }) })); form.reset(); }
  catch (error) { $("#login-error").textContent = error.message; }
});
$("#logout-button").addEventListener("click", async () => {
  try { await api("/auth/logout",{method:"POST"}); location.reload(); }
  catch (error) { showToast(error.message,true); }
});
api("/auth/config").then((config) => { $("#oidc-login").hidden = !config.oidc; }).catch(() => {});
api("/auth/me").then(signedIn).catch(signedOut);
setInterval(() => refresh(true), 3000);
