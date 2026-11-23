import assert from "node:assert/strict";
import {chromium, expect} from "@playwright/test";
import {staticUi} from "./helpers/static-ui.mjs";

const {versionLabel} = await import("../src/lib/version.ts");
assert.equal(versionLabel({version:"x"}),"VaultCenter x");
const full = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678";
assert.equal(versionLabel({version:"0.1.0",revision:"a1b2c3d4e5f6"}),"VaultCenter 0.1.0 · a1b2c3d");
for (const revision of ["unknown","",undefined,"zzzzzzzz","a1b2c3",42]) assert.equal(versionLabel({version:"0.1.0",revision}),"VaultCenter 0.1.0");
for (const bad of [{},{version:""},{version:42},{version:null},null,undefined,"x"]) assert.equal(versionLabel(bad),null);

const ui = await staticUi();
let browser;
try {
  browser = await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM || undefined,args:["--no-sandbox"]});
  const page = await browser.newPage({locale:"fr-FR",viewport:{width:1440,height:1100}});
  const errors = [];
  page.on("pageerror", e => errors.push(e.message));
  let statusBody = {status:"ok",cartridges:0,jobs:0,media_root:"/opt/mhvtl",version:"0.1.0",revision:full};
  let statusCode = 200, statusCalls = 0;
  await page.route("**/auth/**", route => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/auth/config") return route.fulfill({json:{local:true,oidc:false,oidc_label:""}});
    if (path === "/auth/me") return route.fulfill({json:{user:{id:"self",username:"admin",role:"admin",issuer:null,disabled:false},csrf_token:"c"}});
    if (path === "/auth/tokens" || path === "/auth/users") return route.fulfill({json:[]});
    throw Error(`Unexpected auth request: ${path}`);
  });
  await page.route("**/v1/**", route => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith("/telemetry")) return route.fulfill({json:{version:1,vtl_id:"00000000-0000-4000-8000-000000000001",generated_at:null,observed_at:null,source:"mhvtl",status:"unavailable",reason:"not_configured",drives:[],slots:[],media:[]}});
    if (path === "/v1/status") { statusCalls++; return route.fulfill({status:statusCode,json:statusCode === 200 ? statusBody : {error:"boom"}}); }
    const data = {"/v1/vtls":[],"/v1/cartridges":[],"/v1/jobs":[],"/v1/activity":{active:[],recent:[],seen_at:null,unread:0}};
    assert.ok(path in data,`Unexpected API: ${path}`);
    return route.fulfill({json:data[path]});
  });
  const trigger = page.getByRole("button",{name:"Menu du compte",exact:true});
  const line = page.getByText("VaultCenter 0.1.0 · a1b2c3d",{exact:true});
  const load = async () => { statusCalls = 0; await page.goto("about:blank"); await page.goto(`${ui.url}/#account`); await expect(trigger).toBeVisible(); await page.waitForTimeout(500); };

  // success: lazy, once, not a menuitem
  await load();
  assert.equal(statusCalls,0,"no status request before opening the menu");
  await trigger.click();
  await expect(line).toBeVisible();
  assert.deepEqual(await page.getByRole("menuitem").allTextContents(),["Profil","Se déconnecter"]);
  await page.setViewportSize({width:390,height:844});
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth),false);
  await page.setViewportSize({width:1440,height:1100});
  await page.keyboard.press("Escape");
  await trigger.click();
  await expect(line).toBeVisible();
  assert.equal(statusCalls,1,"reopening does not refetch");

  // no version field
  statusBody = {status:"ok",cartridges:0,jobs:0,media_root:"/opt/mhvtl"};
  await load();
  await trigger.click();
  await expect(page.getByRole("menuitem")).toHaveCount(2);
  await expect(page.getByText(/^VaultCenter \d/)).toHaveCount(0);
  assert.ok(statusCalls >= 1);

  // failure then retry
  statusBody = {status:"ok",cartridges:0,jobs:0,media_root:"/opt/mhvtl",version:"0.1.0",revision:full};
  statusCode = 500;
  await load();
  await trigger.click();
  await expect(page.getByRole("menuitem")).toHaveCount(2);
  await expect.poll(() => statusCalls).toBe(1);
  await expect(page.getByText(/^VaultCenter \d/)).toHaveCount(0);
  await page.keyboard.press("Escape");
  statusCode = 200;
  await trigger.click();
  await expect(line).toBeVisible();
  assert.equal(statusCalls,2);
  assert.deepEqual(errors,[]);
  console.log("PASS app version: unit label rules, lazy single fetch, no menuitem, missing version, error retry");
} finally {
  await browser?.close();
  await ui.close();
}
