import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve, extname, sep } from "node:path";
import { chromium, expect } from "@playwright/test";

// Static UI and intercepted API: no requests to an appliance or cloud.
const root = resolve("out");
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".woff2": "font/woff2" };
const server = createServer(async (req, res) => {
  try {
    const path = resolve(root, "." + new URL(req.url, "http://localhost").pathname.replace(/\/$/, "/index.html"));
    if (!path.startsWith(root + sep)) throw Error("outside export");
    const content = await readFile(path);
    res.writeHead(200, { "Content-Type": types[extname(path)] ?? "application/octet-stream" });
    res.end(content);
  } catch {
    res.writeHead(404);
    res.end();
  }
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
let browser;
try {
  browser = await chromium.launch({ executablePath: process.env.COLD_VTL_CHROMIUM, args: ["--no-sandbox"] });
  const page = await browser.newPage({locale:'fr-FR', viewport: { width: 1440, height: 1000 } });
  page.setDefaultTimeout(10000);
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  let cartridges = Array.from({ length: 12 }, (_, index) => ({
    barcode: `TEST${String(index + 1).padStart(3, "0")}L8`,
    state: index < 6 ? "writable" : index < 11 ? "sealed" : "verified",
    generation: 1, bytes_written: 100, capacity_bytes: 1024,
  }));
  let role = "admin";
  let disconnected = false;
  const posts = [];
  let failBarcode = "TEST002L8";
  const jobs = [{ id: "busy", subject: "TEST005L8", kind: "reconcile", status: "running", progress: 0, created_at: "2026-09-17T12:00:00Z", updated_at: "2026-09-17T12:00:00Z", message: null }];
  await page.route("**/auth/**", route => route.fulfill({ json:
    new URL(route.request().url()).pathname === "/auth/config" ? { local: true, oidc: false } :
      { user: { id: "test", username: "test", role, issuer: null, disabled: false }, csrf_token: "test-csrf" },
  }));
  await page.route("**/v1/**", async route => {
    if(new URL(route.request().url()).pathname.endsWith("/telemetry/history"))return route.fulfill({json:emptyThroughputHistory("00000000-0000-4000-8000-000000000001",route.request().url())});
    if (new URL(route.request().url()).pathname.endsWith("/telemetry")) return route.fulfill({json:{version:1,vtl_id:"00000000-0000-4000-8000-000000000001",generated_at:null,observed_at:null,source:"mhvtl",status:"unavailable",reason:"not_configured",drives:[],slots:[],media:[]}});
    if (disconnected) return route.abort();
    const originalPath = new URL(route.request().url()).pathname;
    if (originalPath === "/v1/vtls") return route.fulfill({json:[{id:"00000000-0000-4000-8000-000000000001",name:"VTL principale",lifecycle:"ready",provisioning_revision:1}]});
    // The header bell lists every VTL, so its route is the one global read.
    if (originalPath === "/v1/activity") return route.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
    assert.ok(originalPath.startsWith("/v1/vtls/00000000-0000-4000-8000-000000000001/"), `Unscoped request: ${originalPath}`);
    const path = originalPath.replace("/v1/vtls/00000000-0000-4000-8000-000000000001/","/v1/");
    // Listing old S3 copies must not block sending a local cartridge.
    if (path === "/v1/storage/buckets") return route.fulfill({status:503,json:{error:"Buckets indisponibles"}});
    if (route.request().method() === "POST") {
      assert.equal(route.request().headers()["x-csrf-token"], "test-csrf");
      const match = path.match(/^\/v1\/cartridges\/(TEST\d+L8)\/(seal|export)$/);
      assert.ok(match, `Unexpected mutation ${path}`);
      const [, barcode, kind] = match;
      posts.push({ barcode, kind });
      await new Promise(resolve => setTimeout(resolve, 200));
      if (barcode === failBarcode) return route.fulfill({ status: 409, json: { error: "Maintenance indisponible" } });
      const media = cartridges.find(item => item.barcode === barcode);
      media.state = kind === "seal" ? "sealed" : "uploading";
      return route.fulfill({ json: kind === "seal" ? media : { id: `job-${barcode}` } });
    }
    const data = {
      "/v1/storage/schedule": {revision:1,rotation:"manual",weekday:null,month_day:null,archive_after_days:null,archive_terms_accepted:false,rotation_supported:true,rotation_due:false,period_start:null,active_bucket:"vc-vtl-principale-2026-10-12-ab12"},
      "/v1/client": {schema:1,vtl_id:"00000000-0000-4000-8000-000000000001",revision:0,client:"pbs",initiator:"10.0.0.1",editable:false,message:null},
      "/v1/status": { status: "ok", cartridges: cartridges.length, jobs: jobs.length, media_root: "/test" },
      "/v1/cartridges": cartridges,
      "/v1/jobs": jobs,
      "/v1/cartridges/options": {storage:null},
      "/v1/maintenance": {configured:false},
      "/v1/cartridges/deletion": {ready:false,pending:null,reason:"Maintenance requise"},
    };
    assert.ok(path in data, `Unexpected API request ${path}`);
    return route.fulfill({ json: data[path] });
  });
  await page.goto(`http://127.0.0.1:${server.address().port}/#media`);
  const details = page.getByRole("dialog");
  await page.getByRole("button", {name:"Détails de TEST001L8",exact:true}).click();
  const sendFromDetails = details.getByRole("button", {name:"Envoyer vers S3",exact:true});
  await expect(sendFromDetails).toBeEnabled();
  await expect(sendFromDetails).toBeInViewport();
  if (process.env.S3_EXPORT_SCREENSHOTS) {
    await page.screenshot({path:process.env.S3_EXPORT_SCREENSHOTS+'/details-desktop.png'});
    await page.setViewportSize({width:390,height:844});
    await expect(sendFromDetails).toBeInViewport();
    await page.screenshot({path:process.env.S3_EXPORT_SCREENSHOTS+'/details-mobile.png'});
    assert.equal(await details.evaluate(el => el.scrollWidth > el.clientWidth),false);
    await page.setViewportSize({width:1440,height:1000});
  }
  await sendFromDetails.click();
  await expect(details).toHaveCount(1);
  await expect(details.getByRole("heading", {name:"Envoyer vers S3",exact:true})).toBeVisible();
  await expect(details).toContainText("TEST001L8 · Version 1");
  await expect(details.getByRole("button", {name:"Envoyer vers S3",exact:true})).toBeDisabled();
  assert.equal(posts.length,0);
  await details.getByRole("button", {name:"Annuler",exact:true}).click();
  await page.getByRole("button", {name:"Détails de TEST005L8",exact:true}).click();
  await expect(sendFromDetails).toBeDisabled();
  await expect(details).toContainText("Une opération est en cours sur cette cartouche.");
  await details.getByRole("button", {name:"Fermer",exact:true}).click();
  await page.getByRole("button", { name: "Tableau", exact: true }).click();
  const select = barcode => page.getByRole("checkbox", { name: `Sélectionner ${barcode}`, exact: true });
  const all = page.getByRole("checkbox", { name: "Sélectionner toutes les cartouches de cette page", exact: true });
  const count = page.getByRole("status").filter({ hasText: "sélectionnée" });
  const clear = () => page.getByRole("button", { name: "Tout désélectionner", exact: true }).click();
  const query = page.getByRole("textbox", { name: "Rechercher une cartouche" });
  const dialog = page.getByRole("dialog");
  await select("TEST001L8").check();
  const chassisHeader=page.locator('.lto-chassis');
  for(const name of ['Créer des cartouches','Actualiser','Tableau','Maintenance','Alimentation de la bibliothèque','Envoyer vers S3 (1)','Supprimer (1)']) {
    await expect(chassisHeader.getByRole('button',{name,exact:true})).toBeVisible();
  }
  await expect(chassisHeader.getByRole('button',{name:'Configuration du stockage',exact:true})).toHaveCount(0);
  await expect(chassisHeader.getByRole('textbox',{name:'Rechercher une cartouche'})).toBeVisible();
  await expect(chassisHeader.getByRole('combobox',{name:'Filtrer les cartouches par état'})).toBeVisible();
  await expect(page.getByRole("button", { name: /^Sceller/ })).toHaveCount(0);
  await expect(all).toHaveAttribute("aria-checked", "mixed");
  await all.check();
  await expect(count).toContainText("10 cartouches sélectionnées");
  await page.getByRole("button", { name: "Page suivante des cartouches" }).click();
  await expect(all).not.toBeChecked();
  await select("TEST011L8").check();
  await query.fill("TEST001");
  await expect(count).toContainText("11 cartouches sélectionnées");
  await expect(count).toContainText("10 hors filtre");
  await expect(select("TEST001L8")).toBeChecked();
  await query.fill("");
  await page.getByRole("button", { name: "Sélectionner les 12 résultats" }).click();
  await expect(count).toContainText("12 cartouches sélectionnées");
  await page.getByRole("button", { name: "Envoyer vers S3 (10)", exact: true }).click();
  await expect(dialog.getByRole("list", { name: "Cartouches concernées" }).getByRole("listitem")).toHaveCount(10);
  await expect(dialog).toContainText("La sélection exclut 2 cartouche(s), car leur état ne le permet pas ou une opération est déjà en cours.");
  if (process.env.S3_EXPORT_SCREENSHOTS) {
    await page.screenshot({path:process.env.S3_EXPORT_SCREENSHOTS+'/desktop.png'});
    await page.setViewportSize({width:390,height:844});
    await page.screenshot({path:process.env.S3_EXPORT_SCREENSHOTS+'/mobile.png'});
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
    await page.setViewportSize({width:1440,height:1000});
  }

  await expect(dialog.getByRole("button", { name: "Envoyer vers S3" })).toBeDisabled();
  await dialog.getByRole("button", { name: "Annuler" }).click();
  await clear();
  for (const barcode of ["TEST001L8", "TEST002L8", "TEST003L8"]) await select(barcode).check();
  await page.getByRole("button", { name: "Envoyer vers S3 (3)", exact: true }).click();
  await expect(dialog).toContainText("Destination : vc-vtl-principale-2026-10-12-ab12");
  await expect(dialog.getByRole("checkbox")).toHaveAccessibleName("Ces cartouches sont terminées et déchargées dans PBS.");
  await dialog.getByRole("checkbox").check();
  await dialog.getByRole("button", { name: "Envoyer vers S3" }).dblclick();
  await expect(dialog).toContainText("TEST002L8 : Maintenance indisponible");
  await expect(dialog).toContainText("1 opération(s) confirmée(s)");
  await expect(dialog.getByRole("button", { name: "Envoyer vers S3" })).toBeDisabled();
  assert.deepEqual(posts, [{ barcode: "TEST001L8", kind: "export" }, { barcode: "TEST002L8", kind: "export" }]);
  await dialog.getByRole("button", { name: "Fermer", exact: true }).click();
  await expect(select("TEST001L8")).not.toBeChecked();
  await expect(select("TEST003L8")).toBeChecked();
  await clear();
  failBarcode = null;
  for (const barcode of ["TEST007L8", "TEST008L8"]) await select(barcode).check();
  await page.getByRole("button", { name: "Envoyer vers S3 (2)", exact: true }).click();
  await dialog.getByRole("checkbox").check();
  await dialog.getByRole("button", { name: "Envoyer vers S3" }).click();
  await expect(dialog).not.toBeVisible();
  assert.deepEqual(posts.slice(2), [{ barcode: "TEST007L8", kind: "export" }, { barcode: "TEST008L8", kind: "export" }]);
  await expect(count).toHaveCount(0);
  await select("TEST003L8").check();
  cartridges = cartridges.filter(media => media.barcode !== "TEST003L8");
  await expect(count).toHaveCount(0, { timeout: 12000 });
  // A failed export can resume its generation; unrelated failures cannot.
  const retryMedia = cartridges.find(media => media.barcode === "TEST009L8");
  retryMedia.state = "failed";
  retryMedia.export_resumable = true;
  jobs.unshift({ id: "failed-export", subject: retryMedia.barcode, kind: "export", status: "failed", generation: 1, progress: 0, created_at: "2026-10-02T12:00:00Z", updated_at: "2026-10-02T12:00:00Z", message: "Interrupted" });
  await page.reload();
  await page.getByRole("button", { name: "Tableau", exact: true }).click();
  await page.getByRole("button", { name: "Reprendre l’envoi de TEST009L8", exact: true }).click();
  await expect(dialog).toContainText("même génération");
  await dialog.getByRole("checkbox").check();
  await dialog.getByRole("button", { name: "Envoyer vers S3" }).click();
  await expect(dialog).not.toBeVisible();
  assert.deepEqual(posts.at(-1), { barcode: "TEST009L8", kind: "export" });
  retryMedia.state = "failed";
  jobs[0].kind = "verify";
  retryMedia.export_resumable = false;
  await page.reload();
  await page.getByRole("button", { name: "Tableau", exact: true }).click();
  await expect(page.getByRole("button", { name: "Envoyer TEST009L8 vers S3", exact: true })).toBeDisabled();
  // The default library details can send directly, with the same scoped CSRF-protected export.
  await page.reload();
  await page.getByRole("button", {name:"Détails de TEST006L8",exact:true}).click();
  await sendFromDetails.click();
  await details.getByRole("checkbox").check();
  await details.getByRole("button", {name:"Envoyer vers S3",exact:true}).click();
  await expect(details).not.toBeVisible();
  assert.deepEqual(posts.at(-1),{barcode:"TEST006L8",kind:"export"});
  role = "viewer";
  await page.reload();
  await page.getByRole("button", {name:"Détails de TEST002L8",exact:true}).click();
  await expect(sendFromDetails).toBeDisabled();
  await expect(details).toContainText("Votre compte ne permet pas d’envoyer des cartouches vers S3.");
  await details.getByRole("button", {name:"Fermer",exact:true}).click();
  await page.getByRole("button", { name: "Tableau", exact: true }).click();
  await select("TEST002L8").check();
  await expect(page.getByRole("button", { name: "Envoyer vers S3 (1)", exact: true })).toBeDisabled();
  role = "admin";
  await page.reload();
  await page.getByRole("button", { name: "Tableau", exact: true }).click();
  await select("TEST002L8").check();
  await page.getByRole("button", { name: "Envoyer vers S3 (1)", exact: true }).click();
  cartridges.find(media => media.barcode === "TEST002L8").state = "verified";
  await expect(dialog.getByText("Action indisponible", { exact: true })).toBeVisible({ timeout: 12000 });
  await dialog.getByRole("button", { name: "Annuler" }).click();
  await clear();
  await select("TEST004L8").check();
  disconnected = true;
  await expect(page.getByRole("button", { name: "Envoyer vers S3 (1)", exact: true })).toBeDisabled({ timeout: 12000 });
  await expect(select("TEST004L8")).toBeChecked();
  await page.setViewportSize({ width: 390, height: 844 });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(errors, []);
  console.log("PASS media: page/filter selection, indeterminate state, mixed eligibility, confirmation, CSRF, partial failure, no duplicate requests, bulk exports, removed cartridges, viewer, stale state, disconnect and mobile.");
} finally {
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
