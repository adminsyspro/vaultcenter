import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';
import assert from "node:assert/strict";
import { chromium, expect } from "@playwright/test";
import { staticUi } from "./helpers/static-ui.mjs";
const ui = await staticUi();

// Browser test against the local UI; every API request is intercepted.
const browser = await chromium.launch({ executablePath: process.env.COLD_VTL_CHROMIUM });
try {
  const page = await browser.newPage({locale:'fr-FR', viewport: { width: 1360, height: 950 } });
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  let bound = null, role = "admin", unavailable = false, rejected = true, posts = 0, provisioningPhase="prepared", applyInterrupted=true, activationInterrupted=true;
  let items = [{ id: "00000000-0000-4000-8000-000000000001", name: "VTL principale", lifecycle: "ready", provisioning_revision: 1 }];
  await page.route("**/auth/**", route => route.fulfill({ json: { user: { id: "test", username: "test", role, issuer: null, disabled: false }, csrf_token: "csrf-test", local: true, oidc: false } }));
  await page.route("**/v1/**", async route => {
    if (new URL(route.request().url()).pathname.endsWith("/telemetry")) return route.fulfill({json:{version:1,vtl_id:"00000000-0000-4000-8000-000000000001",generated_at:null,observed_at:null,source:"mhvtl",status:"unavailable",reason:"not_configured",drives:[],slots:[],media:[]}});
    const path = new URL(route.request().url()).pathname;
    if (path === "/v1/vtls") {
      if (unavailable) return route.fulfill({ status: 404, json: {} });
      if (route.request().method() === "POST") {
        posts++;
        assert.equal(route.request().headers()["x-csrf-token"], "csrf-test");
        await new Promise(resolve => setTimeout(resolve, 200));
        if (rejected) return route.fulfill({ status: 409, json: { error: "VTL provisioning is not enabled: isolation and recovery acceptance are required" } });
        const item = { id: "00000000-0000-4000-8000-000000000003", name: route.request().postDataJSON().name, lifecycle: "draft", provisioning_revision: 1 };
        items.push(item);
        return route.fulfill({ status: 201, json: item });
      }
      return route.fulfill({ json: items });
    }
    if(path.endsWith("/bandwidth"))return route.fulfill({json:{vtl_id:path.split("/")[3],revision:1,tape_write_bps:null,s3_bps:null,tape:{state:"stopped",observed_revision:null},s3:{state:"configured"},message:null}});
    if(path.endsWith("/client")){const vtl=path.split("/")[3];
      if(route.request().method()==="PUT"){assert.equal(route.request().headers()["x-csrf-token"],"csrf-test");bound=route.request().postDataJSON().initiator;return route.fulfill({json:{schema:1,vtl_id:vtl,revision:2,client:"veeam",initiator:bound,editable:true,message:null}});}
      return route.fulfill({json:vtl===items[0].id?{schema:1,vtl_id:vtl,revision:0,client:"pbs",initiator:"192.168.1.104",editable:false,message:"Association PBS gérée par la configuration de l’appliance."}:{schema:1,vtl_id:vtl,revision:bound?2:1,client:bound?"veeam":null,initiator:bound,editable:true,message:null}});}
    if(path.endsWith("/settings/tape-storage"))return route.fulfill({json:{vtl_id:path.split("/")[3],mode:"managed",phase:"ready",revision:1,path:"/srv/vaultcenter/tapes/x",capacity:null,operation:null,message:null,data_locked:false,change_allowed:true,change_lock_reason:null}});
    if(path.endsWith("/pbs-connection"))return route.fulfill({json:{vtl_id:path.split("/")[3],revision:0,client:"veeam",provisioning_revision:1,phase:"ready",pbs:null,host:"192.168.1.110",port:3260,iqn:"iqn.test:vtl",changer:{name:null},drive:{name:null},steps:[],message:null}});
    if(path.endsWith("/provisioning"))return route.fulfill({json:{vtl_id:path.split("/")[3],phase:provisioningPhase,qualified:true}});
    for(const action of ["prepare","apply","activate"]) if(path.endsWith("/"+action)){
      assert.equal(route.request().headers()["x-csrf-token"],"csrf-test");
      const value=route.request().postDataJSON();
      if(action==="prepare")assert.equal(value.revision,1);
      if(action==="apply")assert.equal(value.token,"prepared-token");
      if(action==="apply" && applyInterrupted){applyInterrupted=false;provisioningPhase="applying";return route.fulfill({status:409,json:{error:"Application interrompue"}});}
      if(action==="activate" && activationInterrupted){activationInterrupted=false;provisioningPhase="activation_incomplete";return route.fulfill({json:{vtl_id:path.split("/")[3],phase:provisioningPhase}});}
      provisioningPhase=action==="prepare"?"prepared":action==="apply"?"configured":provisioningPhase==="awaiting_pbs"?"active":"awaiting_pbs";
      return route.fulfill({json:{vtl_id:path.split("/")[3],phase:provisioningPhase,...(action==="prepare"?{token:"prepared-token"}:{})}});
    }
    // The header bell lists every VTL, so its route is the one global read.
    if(path==="/v1/activity")return route.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
    const scopedPath = path.replace(/^\/v1\/vtls\/[^/]+(?=\/)/,"/v1");
    if(scopedPath === "/v1/telemetry/history")return route.fulfill({json:emptyThroughputHistory(path.split("/")[3],route.request().url())});
    const data = { "/v1/maintenance":{configured:false}, "/v1/telemetry":{version:1,vtl_id:path.split("/")[3],generated_at:null,observed_at:null,source:"mhvtl",status:"unavailable",reason:"not_configured",drives:[],slots:[],media:[]}, "/v1/status": { status: "ok", cartridges: 0, jobs: 0, media_root: "/test" }, "/v1/cartridges": [], "/v1/jobs": [], "/v1/cartridges/options": {storage:{total_bytes:5*1024**4,free_bytes:2.9*1024**4,reserved_bytes:1.7*1024**4,available_bytes:1.2*1024**4,overhead_per_cartridge:16*1024**2}} };
    assert.ok(scopedPath in data, `Unexpected API request: ${path}`);
    return route.fulfill({ json: data[scopedPath] });
  });
  await page.goto(`${ui.url}/#vtls`);
  await expect(page.getByRole("link", { name: "Bibliothèques virtuelles" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "VTL principale", exact: true })).toBeVisible();
  // Each configured library shows what its local volume can still take.
  const rowGauge = page.getByRole("list", { name: "Bibliothèques virtuelles" }).getByRole("listitem").filter({ hasText: "VTL principale" }).getByRole("button", { name: /^Stockage local : / });
  await expect(rowGauge).toContainText("1,2 Tio à provisionner sur 5 Tio");
  await page.getByRole("button", { name: "Ouvrir VTL principale", exact: true }).click();
  await expect(page.getByRole("dialog")).not.toContainText(items[0].id);
  await expect(page.getByRole("dialog")).toContainText("192.168.1.104");
  await expect(page.getByRole("region",{name:"Bibliothèque en service"})).toContainText("Serveur PBS");
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Créer une VTL", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("button", { name: "Créer la bibliothèque" })).toBeDisabled();
  await dialog.getByLabel("Nom de la bibliothèque").fill(" Sauvegardes production ");
  await dialog.getByRole("button", { name: "Créer la bibliothèque" }).dblclick();
  await expect(dialog).toContainText("Aucune bibliothèque n’a été créée");
  assert.equal(posts, 1);
  await expect(page.getByRole("heading", { name: "Sauvegardes production", exact: true })).toHaveCount(0);
  rejected = false;
  await dialog.getByRole("button", { name: "Créer la bibliothèque" }).click();
  // Creating a VTL opens its window directly on « Mise en service ».
  const created = page.getByRole("dialog", { name: "Sauvegardes production", exact: true });
  await expect(created).toBeVisible();
  await expect(created.getByRole("button", { name: "Mise en service", exact: true })).toHaveAttribute("aria-pressed", "true");
  await expect(created.getByRole("region",{name:"Client de sauvegarde"}).getByLabel("Adresse IP du serveur Veeam")).toBeEditable();
  // The address is saved from the button inside the field; the step then becomes one green line.
  const clientStep = created.getByRole("region",{name:"Client de sauvegarde"});
  await clientStep.getByLabel("Adresse IP du serveur Veeam").fill("192.168.1.105");
  await clientStep.getByRole("button",{name:"Enregistrer",exact:true}).click();
  await expect(clientStep).toContainText("192.168.1.105");
  await expect(clientStep.getByRole("textbox")).toHaveCount(0);
  assert.equal(bound,"192.168.1.105");
  assert.equal(items[1].name, "Sauvegardes production");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("heading", { name: "Sauvegardes production", exact: true })).toBeVisible();
  // A stale preparation (expired token) left by an earlier session must not be reused.
  await page.evaluate(id=>sessionStorage.setItem("vaultcenter-provisioning:"+id,"stale-token"),items[1].id);
  await page.keyboard.press("Escape");
  await page.getByRole("button",{name:"Ouvrir Sauvegardes production"}).click();
  // Reopening the window shows the saved address as a finished step; « Modifier » can be cancelled.
  const reopened = page.getByRole("dialog").getByRole("region",{name:"Client de sauvegarde"});
  await expect(reopened).toContainText("192.168.1.105");await expect(reopened.getByRole("textbox")).toHaveCount(0);
  await reopened.getByRole("button",{name:"Modifier",exact:true}).click();
  await expect(reopened.getByLabel("Adresse IP du serveur Veeam")).toHaveValue("192.168.1.105");
  await reopened.getByRole("button",{name:"Annuler",exact:true}).click();
  await expect(reopened.getByRole("textbox")).toHaveCount(0);await expect(reopened).toContainText("192.168.1.105");
  await page.getByRole("button",{name:"Activer la bibliothèque",exact:true}).click();
  await expect(page.getByRole("dialog")).toContainText("Application interrompue");
  await expect(page.getByRole("button",{name:"Préparer la bibliothèque"})).toHaveCount(0);
  await page.keyboard.press("Escape");
  await page.getByRole("button",{name:"Ouvrir Sauvegardes production"}).click();
  await page.getByRole("button",{name:"Reprendre l’activation"}).click();
  await expect(page.getByRole("button",{name:"Reprendre l’activation"})).toBeEnabled();
  await page.keyboard.press("Escape");
  await page.getByRole("button",{name:"Ouvrir Sauvegardes production"}).click();
  await page.getByRole("button",{name:"Reprendre l’activation"}).click();
  await page.getByRole("button",{name:"Reprendre l’activation"}).click();
  await expect(page.getByRole("dialog").getByRole("region",{name:"Bibliothèque en service"})).toBeVisible();
  for (const tab of ["Mise en service", "Stockage", "Connexion", "Débit"]) await expect(page.getByRole("dialog").getByRole("button",{name:tab,exact:true})).toBeVisible();
  await expect(page.getByRole("dialog").getByRole("list",{name:"Mise en service"})).toHaveCount(0);
  await page.keyboard.press("Escape");
  await page.getByRole("textbox", { name: "Rechercher une bibliothèque" }).fill("inexistante");
  await expect(page.getByText("Aucune bibliothèque ne correspond à la recherche.")).toBeVisible();
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole("textbox", { name: "Rechercher une bibliothèque" }).fill("");
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
  // Deep links open the window on the requested tab.
  await page.goto(`${ui.url}/#vtls?vtl=${items[0].id}&tab=connection`);
  await expect(page.getByRole("dialog",{name:"VTL principale",exact:true}).getByRole("button",{name:"Connexion",exact:true})).toHaveAttribute("aria-pressed","true");
  await page.keyboard.press("Escape");
  await page.goto(`${ui.url}/#vtls?vtl=${items[0].id}&tab=bandwidth`);
  await expect(page.getByRole("dialog",{name:"VTL principale",exact:true}).getByRole("button",{name:"Débit",exact:true})).toHaveAttribute("aria-pressed","true");
  await page.keyboard.press("Escape");
  role = "viewer";
  await page.reload();
  await page.goto(`${ui.url}/#vtls?vtl=${items[0].id}&tab=connection`);
  await expect(page.getByRole("heading", { name: "VTL principale", exact: true })).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.reload();
  await expect(page.getByRole("heading", { name: "VTL principale", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Créer une VTL", exact: true })).toHaveCount(0);
  role = "admin";
  unavailable = true;
  await page.reload();
  await expect(page.getByText("Le service connecté ne propose pas encore", { exact: false })).toBeVisible();
  await expect(page.getByRole("heading", { name: "VTL principale", exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "Créer une VTL", exact: true }).click();
  await expect(page.getByRole("dialog").getByRole("button", { name: "Créer la bibliothèque" })).toBeDisabled();
  assert.deepEqual(errors, []);
  console.log("VTL UI: navigation, details, validation, refusal, persistence response, search, mobile, roles and unavailable API passed.");
} finally { await browser.close(); await ui.close(); }
