import {emptyThroughputHistory} from "./helpers/throughput-history-fixtures.mjs";
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
  browser=await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM || undefined,args:["--no-sandbox"]});
  const page=await browser.newPage({locale:'fr-FR',viewport:{width:1600,height:1100}});
  const errors=[];page.on("pageerror",error=>errors.push(error.message));
  const progressFailures=[];page.on("console",message=>{if(message.text().includes("Maintenance progress read failed"))progressFailures.push(message.text());});
  let role="admin";
  const onlineServices={"tgt.service":"active","vtltape@11.service":"active","vtllibrary@10.service":"active","mhvtl.target":"active"};
  let status={configured:true,phase:"online",ready:false,pbs_locked:false,blocked:false,services:onlineServices,actor:"admin"};
  let jobs=[];
  const events=[{id:1,action:"enter",success:true,actor:"admin",created_at:"2026-09-15 12:00:00",message:""}];
  const mutations=[],bodies=[];
  let release;
  let failTransition=false;
  let delayStatus=false;
  let failStatus=false;
  let telemetryReads=0;
  await page.route("**/auth/**",route=>route.fulfill({json:new URL(route.request().url()).pathname==="/auth/config"?{local:true,oidc:false}:{user:{id:"test",username:role,role,issuer:null,disabled:false},csrf_token:"test-csrf"}}));
  await page.route("**/v1/**",async route=>{
    if(new URL(route.request().url()).pathname.endsWith("/telemetry/history"))return route.fulfill({json:emptyThroughputHistory("00000000-0000-4000-8000-000000000001",route.request().url())});
    if (new URL(route.request().url()).pathname.endsWith("/telemetry")) {telemetryReads++;return route.fulfill({json:{version:1,vtl_id:new URL(route.request().url()).pathname.split("/")[3],generated_at:null,observed_at:null,source:"mhvtl",status:"unavailable",reason:"not_configured",drives:[],slots:[],media:[]}});}
    const originalPath = new URL(route.request().url()).pathname;
    if (originalPath === "/v1/vtls") return route.fulfill({json:[{id:"00000000-0000-4000-8000-000000000001",name:"VTL principale",lifecycle:"ready",provisioning_revision:1}]});
    // The header bell lists every VTL, so its route is the one global read.
    if (originalPath === "/v1/activity") return route.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
    assert.ok(originalPath.startsWith("/v1/vtls/00000000-0000-4000-8000-000000000001/"), `Unscoped request: ${originalPath}`);
    const path = originalPath.replace("/v1/vtls/00000000-0000-4000-8000-000000000001/","/v1/");
    if(route.request().method()==="POST") {
      assert.equal(route.request().headers()["x-csrf-token"],"test-csrf");
      mutations.push(path);
      bodies.push({body:route.request().postData(),type:route.request().headers()["content-type"]??null});
      await new Promise(resolve=>{release=resolve;});
      if(failTransition)return route.fulfill({status:409,json:{error:'Lecteur occupé'}});
      const entering=path.endsWith("/enter");
      status={...status,phase:entering?"maintenance":"online",ready:entering,pbs_locked:entering,blocked:entering,services:Object.fromEntries(Object.keys(onlineServices).map(name=>[name,entering?"inactive":"active"]))};
      return route.fulfill({json:status});
    }
    if(path==="/v1/maintenance" && delayStatus) await new Promise(resolve=>setTimeout(resolve,2500));
    if(path==="/v1/maintenance" && failStatus) return route.fulfill({status:503,json:{error:"Indisponible"}});
    const values={
      "/v1/status":{status:"ok",cartridges:0,jobs:jobs.length,media_root:"/opt/mhvtl"},
      "/v1/cartridges":[],"/v1/jobs":jobs,"/v1/storage/buckets":[],
      "/v1/cartridges/deletion":{ready:status.ready,pending:null},"/v1/cartridges/options":{storage:null},
      "/v1/maintenance":status,"/v1/maintenance/events":events,
      "/v1/reintegrations":{configured:true,pending:status.reintegration_pending?{id:"pending"}:null,operations:status.reintegration_pending?[{id:"pending",barcode:"CVT001L8",generation:1,phase:"replacing",original:{generation:2},created_at:1789459200}]:[]},
    };
    assert.ok(path in values,`Unexpected API ${path}`);
    return route.fulfill({json:values[path]});
  });
  const url=`http://127.0.0.1:${server.address().port}/#media`;
  const enter=()=>page.getByRole("button",{name:"Passer en maintenance",exact:true});
  const leave=()=>page.getByRole("button",{name:"Reprendre le service",exact:true});
  await page.goto(url);
  await expect(page.getByRole("heading",{name:"Bandes et lecteurs",exact:true})).toBeVisible();
  await expect(page.getByRole("link",{name:"Maintenance",exact:true})).toHaveCount(0);
  await expect(page.getByRole("region",{name:"Maintenance",exact:true})).toHaveCount(0);
  const maintenanceButton=page.getByRole("button",{name:"Maintenance",exact:true});
  const powerButton=page.getByRole("button",{name:"Alimentation de la bibliothèque",exact:true});
  await expect(maintenanceButton).toBeVisible();
  const m=await maintenanceButton.boundingBox(),p=await powerButton.boundingBox();
  assert.ok(Math.abs(m.y-p.y)<12 && m.x+m.width<=p.x && p.x-m.x-m.width<32);
  await page.screenshot({path:"/tmp/cold-vtl-maintenance-button-desktop.png",fullPage:true});
  await maintenanceButton.click();
  await expect(page.getByRole("dialog",{name:"Maintenance",exact:true})).toBeVisible();
  await expect(page).toHaveURL(/#media\/maintenance\?vtl=00000000-0000-4000-8000-000000000001$/);
  await expect(page.getByText("Bibliothèque LTO",{exact:true})).toBeVisible();
  await expect(enter()).toBeEnabled();
  const diagnostic=()=>page.getByRole("region",{name:"Diagnostic technique",exact:true});
  await expect(diagnostic()).not.toBeVisible();
  await page.getByText("Détails",{exact:true}).click();
  await expect(diagnostic().getByText("4 / 4 actifs",{exact:true})).toBeVisible();
  await expect(diagnostic().getByText("tgt.service",{exact:true})).toBeVisible();
  await expect(page.getByRole("button",{name:"Réintégration PBS",exact:true})).toHaveCount(0);
  await expect(leave()).toHaveCount(0);
  await expect(page.getByRole("region",{name:"Réintégration PBS",exact:true})).not.toBeVisible();
  await expect(page.getByText("Dernières interventions",{exact:true})).toBeVisible();
  await page.screenshot({path:"/tmp/cold-vtl-maintenance-control.png",fullPage:true});
  await enter().click();
  const dialog=page.getByRole('dialog',{name:'Maintenance',exact:true});
  // The window stays open during the transition and shows its step and progress.
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("button",{name:"Mise en maintenance…",exact:true})).toBeDisabled();
  await expect(dialog.getByRole("progressbar")).toBeVisible();
  await expect(dialog).toContainText("Vous pouvez fermer cette fenêtre. L’opération continue.");
  // The state line and the progress follow the services read during the transition.
  status={...status,phase:"entering",services:{...onlineServices,"tgt.service":"inactive","vtltape@11.service":"inactive"}};
  await expect(dialog).toContainText("Mise en maintenance · 2/4 services actifs");
  await expect(dialog.getByRole("progressbar")).toHaveAttribute("aria-valuenow","50");
  await expect(dialog).toContainText("Les services de la bibliothèque s’arrêtent.");
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await expect(page.locator('.lto-maintenance-ring')).toBeVisible();
  await maintenanceButton.click();
  await expect(dialog.getByRole("progressbar")).toBeVisible();
  assert.deepEqual(mutations,["/v1/maintenance/enter"]);
  await expect(page.locator('.lto-maintenance')).toHaveAttribute('aria-busy','true');
  await expect(page.locator('.lto-maintenance .lto-maintenance-ring')).toBeVisible();
  await expect(page.locator('.lto-maintenance .lucide-wrench')).toBeVisible();
  await expect(page.locator('.lto-power')).toBeDisabled();
  await page.screenshot({path:'/tmp/cold-vtl-maintenance-pending.png',fullPage:true});
  await page.emulateMedia({reducedMotion:'reduce'});
  await expect(page.locator('.lto-maintenance-ring')).toHaveCSS('animation-name','none');
  await page.emulateMedia({reducedMotion:'no-preference'});
  delayStatus=true;
  const readsBeforeTransition=telemetryReads;
  release();
  await expect(dialog).toContainText("En maintenance · 0/4 services actifs");
  await expect(page.locator(".lto-maintenance")).toHaveCSS("color","rgb(255, 106, 0)",{timeout:1000});
  await expect(page.locator(".lto-power")).toHaveAttribute("data-power","off",{timeout:1000});
  await expect(leave()).toBeEnabled({timeout:1000});
  await expect(page.locator('.lto-maintenance')).toHaveAttribute('aria-busy','false');
  await expect(page.locator('.lto-maintenance-ring')).toHaveCount(0);
  await expect.poll(()=>telemetryReads,{timeout:1000}).toBeGreaterThan(readsBeforeTransition);
  delayStatus=false;
  await page.getByText("Détails",{exact:true}).click();
  await expect(diagnostic().getByText("0 / 4 actifs",{exact:true})).toBeVisible();
  await expect(diagnostic().getByText(/en échec/)).toHaveCount(0);
  await expect(leave()).toBeEnabled();
  await expect(enter()).toHaveCount(0);
  await expect(page.getByText("Bibliothèque LTO",{exact:true})).toBeVisible();
  await page.getByText('Détails',{exact:true}).click();
  await expect(diagnostic()).not.toBeVisible();
  await page.screenshot({path:"/tmp/cold-vtl-maintenance-ready.png",fullPage:true});
  await page.setViewportSize({width:390,height:844});
  await page.screenshot({path:"/tmp/cold-vtl-maintenance-ready-mobile.png",fullPage:true});
  await page.setViewportSize({width:1600,height:1100});
  await page.getByText('Détails',{exact:true}).click();
  await leave().click();
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("button",{name:"Reprise des services…",exact:true})).toBeDisabled();
  await expect(dialog).toContainText("Les services de la bibliothèque redémarrent.");
  assert.deepEqual(mutations,["/v1/maintenance/enter","/v1/maintenance/leave"]);
  // A progress read failing every 2 seconds is logged once per transition.
  failStatus=true;
  await expect.poll(()=>progressFailures.length).toBe(1);
  await page.waitForTimeout(4500);
  assert.equal(progressFailures.length,1);
  failStatus=false;
  await expect(page.locator('.lto-maintenance')).toHaveAttribute('aria-busy','true');
  release();
  await expect(enter()).toBeEnabled();
  await expect(page.locator(".lto-maintenance")).toHaveCSS("color","rgb(169, 187, 194)");
  await expect(page.locator('.lto-maintenance')).toHaveAttribute('aria-busy','false');
  failTransition=true;
  await enter().click();
  await expect(page.locator('.lto-maintenance')).toHaveAttribute('aria-busy','true');
  release();
  await expect(dialog).toContainText('Lecteur occupé');
  await expect(page.locator('.lto-maintenance-ring')).toHaveCount(0);
  await expect(enter()).toBeEnabled();
  failTransition=false;
  await expect(page.getByText("Dernières interventions",{exact:true})).toBeVisible();
  await expect(enter()).toBeVisible();
  await page.getByText("Détails",{exact:true}).click();
  status={...status,phase:"failed",reintegration_pending:true,message:"Vérifier la cartouche interrompue",services:{...onlineServices,"tgt.service":"failed"}};
  await page.reload();
  await expect(leave()).toBeDisabled();
  await page.getByText("Détails",{exact:true}).click();
  await expect(diagnostic().getByText("1 en échec",{exact:true})).toBeVisible();
  await expect(page.getByText("Une réintégration est à terminer.",{exact:true})).toBeVisible();await page.getByRole("button",{name:"Voir la réintégration",exact:true}).click();
  await expect(page.getByRole("region",{name:"Réintégration PBS",exact:true})).toBeVisible();
  await expect(page.getByRole("button",{name:"Revenir à la cartouche précédente · CVT001L8",exact:true})).toBeEnabled();
  await page.screenshot({path:"/tmp/cold-vtl-maintenance-recovery.png",fullPage:true});
  status={...status,phase:"online",reintegration_pending:false,message:null,services:onlineServices};
  status={...status,phase:'entering'};
  await page.goto(url);
  await page.reload();
  await expect(page.locator('.lto-maintenance-ring')).toBeVisible();
  // After a reload, a transition reported by the server shows its progress.
  await maintenanceButton.click();
  await expect(dialog.getByRole("progressbar")).toBeVisible();
  await expect(dialog).toContainText("Les services de la bibliothèque s’arrêtent.");
  await expect(dialog.getByRole("button",{name:"Mise en maintenance…",exact:true})).toBeDisabled();
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await page.screenshot({path:'/tmp/cold-vtl-maintenance-ring-desktop.png',fullPage:true});
  await page.setViewportSize({width:390,height:844});
  await page.screenshot({path:'/tmp/cold-vtl-maintenance-ring-mobile.png',fullPage:true});
  await page.setViewportSize({width:1600,height:1100});
  status={...status,phase:'online'};
  await page.goto(url.replace('#media','#media/maintenance'));
  jobs=[{id:"active",kind:"export",subject:"TEST",status:"running",progress:0,created_at:"2026-09-15 12:00:00",updated_at:"2026-09-15 12:00:00",message:null}];
  await page.reload();
  await expect(enter()).toBeDisabled();
  jobs=[];role="viewer";
  await page.reload();
  await expect(enter()).toBeDisabled();
  await expect(page.getByText("La gestion de la maintenance est réservée aux administrateurs.")).toBeVisible();
  await page.setViewportSize({width:390,height:844});
  await page.screenshot({path:"/tmp/cold-vtl-maintenance-mobile.png",fullPage:true});
  await page.getByText("Détails",{exact:true}).click();
  await expect.poll(()=>page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  await expect(diagnostic().getByText("Accès iSCSI",{exact:true})).toBeVisible();
  await expect(diagnostic().getByText("tgt.service",{exact:true})).toBeVisible();
  await page.screenshot({path:"/tmp/cold-vtl-maintenance-diagnostic-mobile.png",fullPage:true});
  status={...status,services:{}};
  await page.reload();
  await page.getByText("Détails",{exact:true}).click();
  await expect(diagnostic().getByText("État indisponible",{exact:true})).toBeVisible();
  await expect(diagnostic().getByText("0 / 0 actifs",{exact:true})).toHaveCount(0);
  await page.getByRole("button",{name:"Fermer",exact:true}).click();
  await expect(maintenanceButton).toBeVisible();
  const mobileMaintenance=await maintenanceButton.boundingBox(),mobilePower=await powerButton.boundingBox();
  assert.ok(Math.abs(mobileMaintenance.y-mobilePower.y)<12 && mobileMaintenance.x+mobileMaintenance.width<=mobilePower.x);
  await page.screenshot({path:"/tmp/cold-vtl-maintenance-button-mobile.png",fullPage:true});
  // Veeam: the operator must confirm that no Veeam job uses the library.
  role="admin";status={...status,client:"veeam",phase:"online",ready:false,pbs_locked:false,blocked:false,services:onlineServices};
  await page.setViewportSize({width:1600,height:1100});
  await page.reload();
  await maintenanceButton.click();
  const idle=page.getByRole("checkbox",{name:"Aucun job Veeam n’utilise cette bibliothèque",exact:true});
  await expect(idle).not.toBeChecked();
  await expect(enter()).toBeDisabled();
  await page.getByText("Détails",{exact:true}).click();
  await expect(page.getByText("Protection des services",{exact:true})).toBeVisible();
  await expect(page.getByText("Lecteur PBS",{exact:true})).toHaveCount(0);
  await idle.check();
  await expect(enter()).toBeEnabled();
  await enter().click();
  await expect.poll(()=>mutations.length).toBe(4);
  assert.equal(mutations[3],"/v1/maintenance/enter");
  assert.deepEqual(JSON.parse(bodies[3].body),{confirm_idle:true});
  assert.match(bodies[3].type,/^application\/json/);
  release();
  await expect(leave()).toBeEnabled();
  assert.equal(bodies[0].body,null);
  assert.deepEqual(errors,[]);
  console.log("PASS simplified maintenance, collapsible history, contextual recovery, in-flight locks, enter/resume, active jobs, viewer access, mobile and Veeam idle confirmation; simulated API only");
} finally {
  await browser?.close();
  await new Promise(resolve=>server.close(resolve));
}
