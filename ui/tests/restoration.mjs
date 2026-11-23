import {selectOption} from './helpers/select.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve, extname, sep } from "node:path";
import { chromium, expect } from "@playwright/test";

// Static build plus simulated API: no appliance or cloud mutation can be sent.
const root = resolve("out");
const types = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
  ".woff2": "font/woff2",
};
const server = createServer(async (req, res) => {
  try {
    const path = resolve(
      root,
      "." +
        new URL(req.url, "http://localhost").pathname.replace(
          /\/$/,
          "/index.html",
        ),
    );
    if (!path.startsWith(root + sep)) throw Error("outside export");
    const content = await readFile(path);
    res.writeHead(200, {
      "Content-Type": types[extname(path)] ?? "application/octet-stream",
    });
    res.end(content);
  } catch {
    res.writeHead(404);
    res.end();
  }
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
let browser;
try {
  browser = await chromium.launch({
    executablePath: process.env.COLD_VTL_CHROMIUM || undefined,
    args: ["--no-sandbox"],
  });
  const page = await browser.newPage({locale:'fr-FR',
    viewport: { width: 1440, height: 1000 },
  });
  page.setDefaultTimeout(10000);
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const generations = [2, 1].map((generation) => ({
    barcode: "CVT001L8",
    generation,
    format_version: generation === 1 ? 1 : 2,
    logical_size_bytes: 4338,
    file_count: 3,
    segment_count: 5,
    local_match: generation === 2,
    manifest_sha256: "a".repeat(64),
  }));
  let copies = generations;
  const activeBucket = "00000000-0000-4000-8000-000000000002";
  const oldBucket = "00000000-0000-4000-8000-000000000003";
  let historical = false;
  let currentActiveBucket = activeBucket;
  const postDestinations = [];
  let jobs = [];
  let failure = false;
  let disconnected = false;
  let generationsFailure = false;
  let role = "admin";
  let reintegrations = { configured: true, pending: null, operations: [] };
  let archiveOverview = {configured:true,operations:[]}, archiveFailure = false;
  let storageFailure = false;
  let storageAccess = {readable:true,writable:true,raw_status:"None"};
  let recoveryFailure = false;
  let storageReport = {
    destination_id: activeBucket, active: true,
    bucket: "example-bucket",
    prefix: "cold-vtl",
    region: "rbx-archive",
    provider: "ovh_cold_archive_v1",
    supported: true,
    raw_status: "None",
    state_label: "Disponible",
    object_access: "Lecture et écriture",
    archive_lock_until: null,
    multipart_uploads: 0,
    checked_at: 1789459200,
    inventory: {
      complete: true,
      objects: 8,
      bytes: 800,
      outside_prefix: 2,
      unrecognized_objects: 0,
      cartridges: [{ barcode: "CVT001L8", objects: 6, bytes: 600 }],
      manifests: [{ barcode: "CVT001L8", generation: 2, key: "key" }],
    },
    checks: [
      {
        id: "scope",
        label: "Objets hors périmètre reconnu",
        level: "blocked",
        detail: "2 objets seraient aussi concernés.",
      },
    ],
    catalogue_generations: [],
  };
  // Per-bucket copy inventory, derived from the same provider state as the storage report.
  const reportAccess = (report) => {
    const readable = report.supported ? ["None","Restored"].includes(report.raw_status) : true;
    return {readable,writable:readable && (!report.supported || report.raw_status === "None"),raw_status:report.raw_status,supported:report.supported};
  };
  const recoveryInventory = (destination_id,bucket,access,items) => ({destination_id,bucket,configuration_revision:1,complete:true,errors:[],access,
    copies:items.map(copy => ({...copy,manifest_sha256:access.readable ? copy.manifest_sha256 : null,manifest_key:`cold-vtl/cartridges/${copy.barcode}/manifests/${copy.generation}.json`}))});
  const providers = [
    {id:"ovh_cold_archive_v1",label:"OVHcloud Cold Archive v1",default_endpoint:"https://s3.rbx-archive.io.cloud.ovh.net",default_region:"rbx-archive",managed_archive:true,description:"Archivage et rappel du bucket entier."},
    {id:"s3_compatible",label:"S3 compatible",default_endpoint:"",default_region:"us-east-1",managed_archive:false,description:"S3 sans archivage Glacier géré."},
  ];
  let settings = {providers,revision:1,configured:true,secret_configured:true,reconcile_required:0,configuration:{provider:"s3_compatible",endpoint:"https://s3.example.test",region:"test-region",bucket:"example-bucket",prefix:"cold-vtl",access_key_id:"test-key"}};
  let settingsFailure = false;
  const settingsCalls = [];
  let release;
  const posts = [];
  await page.route("**/auth/**", (route) =>
    route.fulfill({
      json:
        new URL(route.request().url()).pathname === "/auth/config"
          ? { local: true, oidc: false }
          : {
              user: {
                id: "test",
                username: "admin",
                role,
                issuer: null,
                disabled: false,
              },
              csrf_token: "test-csrf",
            },
    }),
  );
  await page.route("**/v1/**", async (route) => {
    if (new URL(route.request().url()).pathname.endsWith("/telemetry")) return route.fulfill({json:{version:1,vtl_id:new URL(route.request().url()).pathname.split("/")[3],generated_at:null,observed_at:null,source:"mhvtl",status:"unavailable",reason:"not_configured",drives:[],slots:[],media:[]}});
    const originalPath = new URL(route.request().url()).pathname;
    if (originalPath === "/v1/vtls") return route.fulfill({json:[{id:"00000000-0000-4000-8000-000000000001",name:"VTL principale",lifecycle:"ready",provisioning_revision:1,storage:{provider:settings.configuration.provider,bucket:settings.configuration.bucket,prefix:settings.configuration.prefix}}]});
    // The header bell lists every VTL, so its route is the one global read.
    if (originalPath === "/v1/activity") return route.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
    assert.ok(originalPath.startsWith("/v1/vtls/00000000-0000-4000-8000-000000000001/"), `Unscoped request: ${originalPath}`);
    const path = originalPath.replace("/v1/vtls/00000000-0000-4000-8000-000000000001/","/v1/");
    const json = (body) => route.fulfill({ json: body });
    if (disconnected) return route.abort();
    if (path.startsWith("/v1/settings/s3")) {
      if (route.request().method() === "GET") return json(settings);
      assert.equal(route.request().headers()["x-csrf-token"],"test-csrf");
      const body = route.request().postDataJSON(); settingsCalls.push({path,body,method:route.request().method()});
      if (settingsFailure) return route.fulfill({status:409,json:{error:"Connexion S3 refusée"}});
      await new Promise(resolve => setTimeout(resolve,100));
      if (route.request().method() === "PUT") {
        const {revision,secret_access_key,...configuration} = body;
        settings = {...settings,revision:revision+1,configuration,reconcile_required:1};
        return json({status:"applied",message:"Configuration appliquée."});
      }
      return json({status:"ok",read_tested:true,write_tested:false});
    }
    if (route.request().method() === "POST") {
      assert.equal(route.request().headers()["x-csrf-token"], "test-csrf");
      postDestinations.push(new URL(route.request().url()).searchParams.get("destination_id"));
      posts.push({ path, body: route.request().postDataJSON() });
      if (failure)
        return route.fulfill({
          status: 409,
          json: { error: "restore destination already exists" },
        });
      await new Promise((resolve) => {
        release = resolve;
      });
      return json({ id: "restore-test", status: "queued" });
    }
    if (path === "/v1/storage/buckets") return json([
      {id:activeBucket,bucket:"example-bucket",active:currentActiveBucket === activeBucket,configuration_revision:1,copy_count:2,observed_status:"None",rotation_phase:null},
      ...(historical ? [{id:oldBucket,bucket:"archive-2026-08",active:currentActiveBucket === oldBucket,configuration_revision:1,copy_count:1,observed_status:"Restored",rotation_phase:"switched"}] : []),
    ]);
    if (historical && new URL(route.request().url()).searchParams.get("destination_id") === oldBucket) {
      if (path.endsWith("/generations")) return json([{...generations[1],logical_size_bytes:8192}]);
      if (path === "/v1/storage/access") return json({readable:true,writable:false,raw_status:"Restored"});
      if (path === "/v1/storage") return json({...storageReport,bucket:"archive-2026-08",destination_id:oldBucket,active:false,raw_status:"Restored"});
      if (path === "/v1/storage/archive") return json({configured:true,operations:[]});
      if (path === "/v1/storage/recovery") return json(recoveryInventory(oldBucket,"archive-2026-08",{readable:true,writable:false,raw_status:"Restored",supported:true},[{...generations[1],logical_size_bytes:8192}]));
    }
    if (path.endsWith("/generations"))
      return generationsFailure
        ? route.fulfill({
            status: 500,
            json: { error: "Catalogue indisponible" },
          })
        : json(copies);
    if (path === "/v1/status")
      return json({
        status: "ok",
        cartridges: 1,
        jobs: jobs.length,
        media_root: "/opt/mhvtl",
      });
    if (path === "/v1/cartridges")
      return json([
        {
          barcode: "CVT001L8",
          generation: 2,
          state: "verified",
          bytes_written: 4338,
          capacity_bytes: 2147483648,
        },
      ]);
    if (path === "/v1/jobs") return json(jobs);
    if (path === "/v1/telemetry/history") return json(emptyThroughputHistory("00000000-0000-4000-8000-000000000001",route.request().url()));
    if (path === "/v1/cartridges/deletion") return json({pending:null});
    if (path === "/v1/maintenance") return json({configured:false});
    if (path === "/v1/storage/access") return json(storageAccess);
    if (path === "/v1/storage/archive") return archiveFailure ? route.fulfill({status:503,json:{error:"Suivi OVH momentanément indisponible."}}) : json(archiveOverview);
    if (path === "/v1/storage/recovery")
      return recoveryFailure
        ? route.fulfill({status:503,json:{error:"Bucket inaccessible"}})
        : json(recoveryInventory(activeBucket,"example-bucket",reportAccess(storageReport),copies));
    if (path === "/v1/storage")
      return storageFailure
        ? route.fulfill({
            status: 409,
            json: { error: "Délai de lecture dépassé" },
          })
        : json(storageReport);
    if (path === "/v1/settings/tape-storage") return json({vtl_id:"00000000-0000-4000-8000-000000000001",mode:"managed",phase:"ready",revision:1,path:"/srv/tapes",capacity:null,operation:null,message:null,data_locked:false,change_allowed:true,change_lock_reason:null});
    if (path === "/v1/storage/buckets/discover") return json({buckets:[],complete:true,error:null,revision:1,suggested_bucket:"vc-vtl-principale-20261012",creation_supported:true,creations:[]});
    if (path === "/v1/storage/connection") return json({revision:1,configured:true,secret_configured:true,configuration:{provider:settings.configuration.provider,endpoint:settings.configuration.endpoint,region:settings.configuration.region,prefix:settings.configuration.prefix,access_key_id:settings.configuration.access_key_id}});
    if (path === "/v1/storage/schedule") return json({revision:0,rotation:"manual",weekday:null,month_day:null,archive_after_days:null,archive_terms_accepted:false,rotation_supported:true,rotation_due:false,period_start:null,active_bucket:"example-bucket"});
    if (path === "/v1/reintegrations") return json(reintegrations);
    if (path === "/v1/cartridges/options") return json({storage:null});
    throw Error("Unexpected API route: " + path);
  });
  const url = `http://127.0.0.1:${server.address().port}/`;
  await page.goto(url + "#overview");
  await expect(page.getByRole("heading",{name:"Vue d’ensemble",exact:true})).toBeVisible();
  const menu = page.getByRole('navigation',{name:'Navigation de la bibliothèque'});
  assert.deepEqual(await menu.getByRole("link").allTextContents(),["Bibliothèque","Activité","Stockage S3","Réglages"]);
  await expect(page.getByText("Activité récente",{exact:true})).toBeVisible();
  await page.screenshot({path:"test-results/dashboard-desktop.png",fullPage:true});
  await page.getByRole("button",{name:"Changer le thème"}).click();
  await page.screenshot({path:"test-results/dashboard-dark.png",fullPage:true});
  await page.getByRole("button",{name:"Changer le thème"}).click();
  await page.setViewportSize({width:390,height:844});
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
  await page.getByRole("link",{name:"Bibliothèque",exact:true}).click();
  await expect(page.getByText("Inventaire des cartouches",{exact:true})).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.setViewportSize({width:1440,height:1000});
  console.log("PASS grouped navigation order, dashboard focus, themes and mobile menu");
  const writesBeforeStorage = posts.length;
  const bucketList = page.getByRole("list", { name: "Buckets", exact: true });
  const bucketRow = (name) => bucketList.getByRole("listitem").filter({ hasText: name });
  const manageBucket = (name) => page.getByRole("button", { name: `Gérer le bucket ${name}`, exact: true });
  const bucketDialog = page.getByRole("dialog");
  const openBucket = async (name) => {
    await manageBucket(name).click();
    await expect(bucketDialog.getByRole("heading", { name, exact: true })).toBeVisible();
    await expect(bucketDialog.getByText("Lecture de l’état et de l’inventaire du stockage…", { exact: true })).toHaveCount(0);
  };
  const closeBucket = async () => {
    await bucketDialog.getByRole("button", { name: "Fermer", exact: true }).last().click();
    await expect(bucketDialog).toHaveCount(0);
  };
  role = "viewer";
  await page.goto(url + "#storage");
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "Stockage S3", exact: true }),
  ).toBeVisible();
  await expect(bucketRow("example-bucket").getByText("example-bucket", { exact: true })).toBeVisible();
  await expect(bucketRow("example-bucket")).toContainText("Actif");
  await expect(bucketRow("example-bucket")).toContainText("Disponible sur S3");
  await expect(manageBucket("example-bucket")).toBeVisible();
  await openBucket("example-bucket");
  await expect(
    page.getByRole("button", { name: /Archiver|Rappeler/ }),
  ).toHaveCount(0);
  await closeBucket();
  storageReport = {
    ...storageReport,
    raw_status: "Restored",
    state_label: "Rappel terminé",
    object_access: "Lecture seule",
  };
  await page.reload();
  await expect(bucketRow("example-bucket")).toContainText("Disponible · lecture seule");
  await openBucket("example-bucket");
  await expect(bucketDialog.getByText("Rappelé · lecture seule", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: /Archiver|Rappeler/ })).toHaveCount(0);
  await closeBucket();
  storageFailure = true;
  recoveryFailure = true;
  await page.reload();
  await expect(bucketRow("example-bucket")).toContainText("Lecture impossible");
  await expect(page.getByText("Inventaire partiel", { exact: true })).toBeVisible();
  await openBucket("example-bucket");
  await expect(
    bucketDialog.getByText("État du stockage non confirmé", { exact: true }),
  ).toBeVisible();
  await expect(bucketDialog.getByText("Données prêtes à être restaurées", { exact: true })).toHaveCount(0);
  await closeBucket();
  storageFailure = false;
  recoveryFailure = false;
  storageReport = {
    ...storageReport,
    raw_status: null,
    state_label: "État inconnu",
    object_access: "Accès non confirmé",
    inventory: null,
  };
  await page.reload();
  await expect(bucketRow("example-bucket")).toContainText("Accès à vérifier");
  await openBucket("example-bucket");
  await expect(bucketDialog.getByText("État du bucket à vérifier", { exact: true })).toBeVisible();
  await closeBucket();
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(manageBucket("example-bucket")).toBeVisible();
  assert.equal(
    await page.evaluate(
      () => document.documentElement.scrollWidth > innerWidth,
    ),
    false,
  );
  await page.screenshot({
    path: "test-results/storage-mobile.png",
    fullPage: true,
  });
  await page.setViewportSize({ width: 1440, height: 1000 });
  assert.equal(posts.length, writesBeforeStorage);
  assert.deepEqual(errors, []);
  console.log(
    "PASS read-only storage for viewer, bucket list, recalled read-only state, unknown/error recovery and mobile",
  );

  role = "admin";
  storageReport = {...storageReport,raw_status:"None",state_label:"Disponible",object_access:"Lecture et écriture",managed:false,next_bucket:"vc-vtl-principale-2026-10-13-ab12",
    inventory:{...(storageReport.inventory ?? {}),complete:true,objects:6,bytes:1024,cartridges:[{barcode:"CVT001L8",objects:6,bytes:1024}],manifests:[]},
    catalogue_generations:[{barcode:"CVT001L8",generation:2,key:"k2",checked_at:"2026-10-01 10:00:00"},{barcode:"CVT001L8",generation:1,key:"k1",checked_at:"2026-09-01 10:00:00"}]};
  jobs = [];
  await page.goto(url + "#storage");
  await page.reload();
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(bucketRow("example-bucket")).toContainText("Actif");
  await expect(bucketRow("example-bucket")).toContainText("Disponible sur S3");
  await expect(page.getByRole("region",{name:"Copies S3",exact:true}).getByRole("button",{name:"Réintégrer",exact:true}).first()).toBeEnabled();
  await openBucket("example-bucket");
  await expect(bucketDialog).toContainText("Disponible · bucket actif");
  await expect(bucketDialog.getByRole("button",{name:"Actualiser",exact:true})).toBeHidden();
  await bucketDialog.getByRole("button",{name:"Archiver sur bande…",exact:true}).click();
  const confirmation = bucketDialog.getByRole("region",{name:"Archiver sur bande",exact:true});
  await expect(confirmation.getByRole("heading",{name:"Archiver sur bande",exact:true})).toBeVisible();
  await expect(bucketDialog).toContainText(/Cartouches : 1 · volume : .+ · copies enregistrées : 2/);
  await expect(confirmation).toContainText(/Prochains exports\s*vc-vtl-principale-2026-10-13-ab12/);
  await expect(confirmation).toContainText("1 Tio minimum pendant 180 jours");
  await expect(bucketDialog.getByText("Dernières opérations",{exact:true})).toHaveCount(0);
  await expect(confirmation.getByRole("textbox")).toHaveCount(0);
  const submitArchive = confirmation.getByRole("button",{name:"Archiver",exact:true});
  await expect(submitArchive).toBeDisabled();
  await confirmation.getByLabel("J’ai compris les conditions OVH").check();
  await expect(submitArchive).toBeDisabled();
  await confirmation.getByLabel("Ce bucket est réservé à cette VTL.").check();
  await expect(submitArchive).toBeEnabled();
  await page.screenshot({path:"test-results/archive-confirmation-mobile.png",fullPage:true});
  await submitArchive.click();
  await expect.poll(() => posts.length).toBe(writesBeforeStorage+1);
  assert.deepEqual(posts.at(-1),{path:"/v1/storage/archive",body:{confirm_bucket:"example-bucket",acknowledge_billing:true,exclusive_bucket:true,next_bucket:"vc-vtl-principale-2026-10-13-ab12"}});
  await expect(submitArchive).toBeDisabled();
  archiveOverview.operations = [{operation:"archive",bucket:"example-bucket",job_id:"archive-submit",prepare_job_id:"archive-submit",phase:"uncertain",observed_status:null,observed_at:null,restored_until:null}];
  release();
  await expect(confirmation).toHaveCount(0);
  await expect(bucketDialog).toContainText("Archivage en cours chez OVH");
  await expect(bucketDialog.getByRole("progressbar")).toBeVisible();
  await expect(bucketDialog.getByRole("button",{name:"Archiver sur bande…",exact:true})).toHaveCount(0);
  await bucketDialog.getByText("Dernières opérations",{exact:true}).click();
  await expect(bucketDialog.getByRole("button",{name:"Vérifier la demande chez OVH",exact:true})).toBeEnabled();
  await expect(bucketDialog.getByText("Le résultat de la demande est incertain. Vérifiez-le chez OVH.")).toBeVisible();
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth),false);
  await closeBucket();
  await page.setViewportSize({ width: 1440, height: 1000 });
  assert.deepEqual(errors,[]);
  console.log("PASS archive from the bucket window: one-line state, real-risk confirmation without typed name, exclusive checkbox for an attached bucket, CSRF, duplicate suppression, progress and mobile");
  archiveOverview.operations = [];
  storageReport = {...storageReport,raw_status:"Archived",managed:true};
  jobs = [];
  archiveFailure = true;
  await page.reload();
  await expect(bucketRow("example-bucket")).toContainText("Archivé · lecture seule");
  await openBucket("example-bucket");
  await expect(bucketDialog).toContainText("Archivé sur bande");
  // A failed read of the OVH operations is cleared by the next successful one.
  await expect(bucketDialog).toContainText("Suivi OVH momentanément indisponible.");
  archiveFailure = false;
  jobs = [{id:"refresh-marker",kind:"create",subject:"CVT009L8",status:"completed",progress:100,message:null,created_at:"2026-10-13 10:00:00",updated_at:"2026-10-13 10:00:00"}];
  await expect(bucketDialog).not.toContainText("Suivi OVH momentanément indisponible.",{timeout:15000});
  jobs = [];
  await bucketDialog.getByRole("button",{name:"Rappeler le bucket…",exact:true}).click();
  const recallConfirmation = bucketDialog.getByRole("region",{name:"Rappeler le bucket",exact:true});
  const recall = recallConfirmation.getByRole("button",{name:"Rappeler",exact:true});
  await expect(recall).toBeDisabled();
  await expect(recallConfirmation.getByRole("textbox")).toHaveCount(0);
  await recallConfirmation.getByLabel("J’accepte les frais de rappel du bucket entier.").check();
  await expect(recall).toBeEnabled();
  const priorRecall = posts.length;
  await recall.click();
  await expect.poll(() => posts.length).toBe(priorRecall+1);
  assert.deepEqual(posts.at(-1),{path:"/v1/storage/recall",body:{confirm_bucket:"example-bucket",acknowledge_retrieval_cost:true}});
  await expect(recall).toBeDisabled();
  archiveOverview.operations = [{operation:"recall",bucket:"example-bucket",job_id:"recall-submit",prepare_job_id:"recall-submit",phase:"accepted",observed_status:"Restoring",observed_at:new Date().toISOString(),restored_until:null}];
  storageReport.raw_status = "Restoring";
  release();
  await expect(recallConfirmation).toHaveCount(0);
  await page.reload();
  await expect(bucketRow("example-bucket")).toContainText("Rappel en cours");
  await openBucket("example-bucket");
  await expect(bucketDialog).toContainText("Rappel en cours chez OVH");
  await expect(bucketDialog.getByRole("button",{name:"Rappeler le bucket…",exact:true})).toHaveCount(0);
  storageReport.raw_status = "Restored";
  archiveOverview.operations[0].phase = "restored";
  archiveOverview.operations[0].restored_until = "2026-11-12T10:00:00Z";
  await page.reload();
  await openBucket("example-bucket");
  await expect(bucketDialog).toContainText("Rappelé · lecture seule jusqu’au 12/11");
  await expect(page.getByRole("link",{name:"Restaurer une cartouche…"})).toBeVisible();
  await expect(bucketDialog.getByRole("button",{name:"Archiver sur bande…",exact:true})).toHaveCount(0);
  await closeBucket();
  storageAccess = {readable:false,writable:false,raw_status:"Archived"};
  console.log("PASS recall with one cost checkbox, progress and read-only window end");
  await page.goto(url + "#acceptance");
  await expect(page.getByRole("heading",{name:"Bandes et lecteurs",exact:true})).toBeAttached();
  await expect(menu.getByRole("link",{name:"Bibliothèque",exact:true})).toHaveAttribute("aria-current","page");
  await expect(page.getByRole("link",{name:"Guide de recette",exact:true})).toHaveCount(0);
  assert.deepEqual(errors,[]);
  console.log("PASS removed acceptance page falls back to library");
  storageFailure = true;
  await page.goto(url + "#storage");
  await expect(page).toHaveURL(/#storage(?:\?|$)/);
  await expect(page.getByRole("heading",{name:"Stockage S3",exact:true})).toBeVisible();
  await page.reload();
  await openBucket("example-bucket");
  await expect(bucketDialog.getByText("État du stockage non confirmé",{exact:true})).toBeVisible();
  await closeBucket();
  storageFailure = false;
  // S3 configuration lives in the VTL window; viewers only reach the read-only S3 page.
  await page.goto(url + "#s3settings");
  await expect(page).toHaveURL(/#vtls\?vtl=00000000-0000-4000-8000-000000000001$/);
  await expect(page.getByRole("dialog",{name:"VTL principale",exact:true}).getByRole("region",{name:"Connexion S3",exact:true})).toBeVisible();
  await page.keyboard.press("Escape");
  role = "viewer";
  await page.reload();
  await page.goto(url + "#s3settings");
  await expect(page).toHaveURL(/#storage\?vtl=/);
  await expect(page.getByRole("heading",{name:"Stockage S3",exact:true})).toBeVisible();
  await expect(page.getByLabel("Secret key",{exact:true})).toHaveCount(0);
  assert.deepEqual(errors,[]);
  console.log("PASS storage error state, S3 settings route to the VTL window and viewer restriction");
  role = "admin"; historical = true; jobs = []; copies = generations; storageAccess = {readable:true,writable:true,raw_status:"None"};
  storageReport = {...storageReport,raw_status:"None",maintenance:{ready:true,lease:"test-lease"}};
  await page.goto(url+"#storage");
  await page.reload();
  const copyRegion = page.getByRole("region",{name:"Copies S3",exact:true});
  await expect(bucketRow("archive-2026-08")).toContainText("Historique");
  await expect(bucketRow("archive-2026-08")).toContainText("Disponible · lecture seule");
  await expect(bucketRow("example-bucket")).toContainText("Actif");
  // Each bucket lists its own copies: no bucket column, no bucket filter.
  await expect(bucketRow("archive-2026-08")).toContainText("1 copie · 8 Kio");
  await expect(bucketRow("archive-2026-08").getByRole("rowheader")).toHaveCount(1);
  await expect(bucketRow("example-bucket").getByRole("rowheader")).toHaveCount(2);
  await expect(copyRegion.getByRole("cell",{name:"archive-2026-08",exact:true})).toHaveCount(0);
  await expect(page.getByLabel("Bucket",{exact:true})).toHaveCount(0);
  await openBucket("archive-2026-08");
  await expect(page.getByRole("button",{name:"Archiver sur bande…",exact:true})).toHaveCount(0);
  await expect(page.getByText("Rappelé · lecture seule",{exact:true})).toBeVisible();
  await closeBucket();
  console.log("PASS historical bucket listing, copies per bucket and storage details");
  currentActiveBucket = oldBucket;
  jobs = [{id:"rotation-complete",kind:"archive_submit",destination_id:activeBucket,subject:"example-bucket",status:"completed",progress:100,message:"{}",created_at:new Date().toISOString(),updated_at:new Date().toISOString()}];
  await expect(bucketRow("archive-2026-08")).toContainText("Actif",{timeout:15000});
  await expect(bucketRow("example-bucket")).toContainText("Historique");
  // A block folded by the user stays folded when the list is read again.
  const foldOld = bucketRow("archive-2026-08").getByRole("button",{name:"archive-2026-08",exact:true});
  await foldOld.click();
  await expect(foldOld).toHaveAttribute("aria-expanded","false");
  jobs = [{...jobs[0],id:"observation-complete",kind:"archive_refresh"}];
  await page.waitForTimeout(5500);
  await expect(foldOld).toHaveAttribute("aria-expanded","false");
  await expect(bucketRow("archive-2026-08").getByRole("rowheader")).toHaveCount(0);
  await foldOld.click();
  currentActiveBucket = activeBucket; jobs = [];
  console.log("PASS bucket list follows rotation while a folded bucket stays folded");

  storageReport={...storageReport,provider:"s3_compatible",provider_label:"S3 compatible",supported:false,raw_status:null,state_label:"S3 compatible",object_access:"Accès selon les droits S3"};
  await page.goto(url+"?provider=generic#storage");
  await expect(copyRegion.getByRole("rowheader",{name:"CVT001L8",exact:true}).first()).toBeVisible();
  await openBucket("example-bucket");
  await expect(page.getByText("Archivage géré indisponible",{exact:true})).toBeVisible();
  await expect(page.getByRole("button",{name:"Archivage",exact:true})).toHaveCount(0);
  await expect(page.getByRole("button",{name:"Rappel",exact:true})).toHaveCount(0);
  await expect(page.getByRole("button",{name:"Archiver sur bande…",exact:true})).toHaveCount(0);
  await closeBucket();
  console.log("PASS provider selection, provider defaults, persistence and generic S3 without OVH archive actions");



} finally {
  await browser?.close();
  await new Promise((resolve) => server.close(resolve));
}
