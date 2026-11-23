import {selectOption} from './helpers/select.mjs';
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
  browser = await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM || undefined,args:["--no-sandbox"]});
  const page = await browser.newPage({locale:'fr-FR',viewport:{width:1440,height:1100}});
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  let user = {id:"self",username:"admin",role:"admin",issuer:null,disabled:false};
  let users = [user,{id:"reader",username:"lecteur",role:"viewer",issuer:null,disabled:false},{id:"sso",username:"operateur-sso",role:"viewer",issuer:"https://identity.example.test",disabled:false}];
  let tokens = [{id:"existing",name:"CLI exploitation",expires_at:1798761600}];
  let passwordFailure = true;
  const mutations = [];
  const reads = [];
  await page.route("**/auth/**", async route => {
    const req = route.request();
    const path = new URL(req.url()).pathname;
    const json = value => route.fulfill({json:value});
    if (req.method() === "GET") {
      reads.push(path);
      if (path === "/auth/config") return json({local:true,oidc:true,oidc_label:"Keycloak"});
      if (path === "/auth/me") return json({user,csrf_token:"account-csrf"});
      if (path === "/auth/tokens") return json(tokens);
      if (path === "/auth/users") return json(users);
    }
    assert.equal(req.headers()["x-csrf-token"],"account-csrf");
    const body = req.postDataJSON();
    mutations.push({path,method:req.method(),body});
    if (path === "/auth/logout") return json({status:"ok"});
    if (path === "/auth/password") {
      if (passwordFailure) return route.fulfill({status:400,json:{error:"Mot de passe actuel incorrect."}});
      return json({status:"ok"});
    }
    if (path === "/auth/tokens" && req.method() === "POST") {
      tokens.push({id:"new",name:body.name,expires_at:1798761600});
      return json({token:"simulated-token-only"});
    }
    if (path.startsWith("/auth/tokens/") && req.method() === "DELETE") {
      tokens = tokens.filter(token => token.id !== path.split("/").at(-1));
      return json({status:"ok"});
    }
    if (path === "/auth/users" && req.method() === "POST") {
      users.push({id:"created",username:body.username,role:body.role,issuer:null,disabled:false});
      return json({status:"ok"});
    }
    if (path.startsWith("/auth/users/")) {
      users = users.map(item => item.id === path.split("/").at(-1) ? {...item,...body} : item);
      return json({status:"ok"});
    }
    throw Error(`Unexpected auth request: ${req.method()} ${path}`);
  });
  await page.route("**/v1/**", route => {
    if (new URL(route.request().url()).pathname.endsWith("/telemetry")) return route.fulfill({json:{version:1,vtl_id:"00000000-0000-4000-8000-000000000001",generated_at:null,observed_at:null,source:"mhvtl",status:"unavailable",reason:"not_configured",drives:[],slots:[],media:[]}});
    assert.equal(route.request().method(),"GET");
    const originalPath = new URL(route.request().url()).pathname;
    if (originalPath === "/v1/vtls") return route.fulfill({json:[]});
    const path = originalPath;
    const data = {"/v1/status":{status:"ok",cartridges:0,jobs:0,media_root:"/opt/mhvtl"},"/v1/cartridges":[],"/v1/jobs":[],"/v1/activity":{active:[],recent:[],seen_at:null,unread:0}};
    assert.ok(path in data,`Unexpected API: ${path}`);
    return route.fulfill({json:data[path]});
  });
  await page.goto(`http://127.0.0.1:${server.address().port}/#account`);
  await expect(page.getByLabel("Rôle de admin",{exact:true})).toBeVisible();
  const prefix = process.env.COLD_VTL_CAPTURE_PREFIX || "account";
  await page.screenshot({path:`/tmp/cold-vtl-${prefix}-desktop.png`,fullPage:true});
  await page.setViewportSize({width:390,height:844});
  await page.screenshot({path:`/tmp/cold-vtl-${prefix}-mobile.png`,fullPage:true});
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth),false);
  await page.setViewportSize({width:1440,height:1100});
  const menuTrigger = page.getByRole("button",{name:"Menu du compte",exact:true});
  const profile = page.getByRole("region",{name:"Profil",exact:true});
  await expect(profile).toHaveCount(0);
  await expect(page.locator("header").getByText(/Environnement de|lab/i)).toHaveCount(0);
  await menuTrigger.click();
  assert.deepEqual(await page.getByRole("menuitem").allTextContents(),["Profil","Se déconnecter"]);
  await page.screenshot({path:"/tmp/cold-vtl-account-menu-desktop.png",fullPage:true});
  await page.setViewportSize({width:390,height:844});
  await page.screenshot({path:"/tmp/cold-vtl-account-menu-mobile.png",fullPage:true});
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth),false);
  await page.keyboard.press("Escape");
  await expect(menuTrigger).toBeFocused();
  await menuTrigger.press("Enter");
  reads.length = 0;
  await page.getByRole("menuitem",{name:"Profil",exact:true}).click();
  await expect(page.getByRole("heading",{name:"Profil",level:1,exact:true})).toBeVisible();
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(profile.getByText("admin",{exact:true})).toBeVisible();
  await expect(profile.getByLabel("Mot de passe actuel",{exact:true})).toBeVisible();
  await expect(profile.getByLabel("Nouveau mot de passe",{exact:true})).toBeVisible();
  assert.deepEqual(await page.locator("main h2").allTextContents(),["Profil","Jetons API et CLI"]);
  await expect(page.getByRole("button",{name:"Révoquer CLI exploitation",exact:true})).toBeVisible();
  assert.ok(!reads.includes("/auth/users") && reads.includes("/auth/tokens"));
  await page.screenshot({path:"/tmp/cold-vtl-profile-mobile.png",fullPage:true});
  await page.setViewportSize({width:1440,height:1100});
  await page.screenshot({path:"/tmp/cold-vtl-profile-desktop.png",fullPage:true});
  await page.getByLabel("Nom du jeton",{exact:true}).fill("CLI personnel");
  await page.getByRole("button",{name:"Créer un jeton",exact:true}).click();
  await expect(page.getByLabel("Jeton à copier maintenant")).toHaveValue("simulated-token-only");
  assert.deepEqual(mutations.at(-1),{path:"/auth/tokens",method:"POST",body:{name:"CLI personnel",days:30}});
  await page.getByRole("button",{name:"Révoquer CLI personnel",exact:true}).click();
  await expect(page.getByRole("button",{name:"Révoquer CLI personnel",exact:true})).toHaveCount(0);
  await expect(page.getByLabel("Jeton à copier maintenant")).toHaveCount(0);
  reads.length = 0;
  await page.getByRole("link",{name:"Utilisateurs",exact:true}).click();
  await expect(page.getByLabel("Rôle de admin",{exact:true})).toBeDisabled();
  assert.deepEqual(await page.locator("main h2").allTextContents(),["Administration des utilisateurs"]);
  assert.ok(reads.includes("/auth/users") && !reads.includes("/auth/tokens"));
  await expect(page.getByLabel("Rôle de operateur-sso",{exact:true})).toBeDisabled();
  const newRole=page.getByLabel("Rôle du nouvel utilisateur",{exact:true});
  assert.equal(await newRole.evaluate(element=>new FormData(element.form).get('role')), 'viewer');
  await selectOption(newRole, 'admin');
  await page.getByLabel("Nouvel utilisateur local",{exact:true}).fill("nouveau");
  await page.getByLabel("Mot de passe initial",{exact:true}).fill("initial-test-password");
  await page.getByRole("button",{name:"Créer l’utilisateur",exact:true}).click();
  await expect(page.getByLabel("Rôle de nouveau",{exact:true})).toBeVisible();
  assert.deepEqual(mutations.at(-1).body,{username:"nouveau",password:"initial-test-password",role:"admin"});
  await selectOption(page.getByLabel("Rôle de lecteur",{exact:true}), "admin");
  await expect(page.getByLabel("Rôle de lecteur",{exact:true})).toHaveAttribute('data-value', "admin");
  assert.deepEqual(mutations.at(-1).body,{role:"admin",disabled:false});
  await menuTrigger.click();
  await page.getByRole("menuitem",{name:"Profil",exact:true}).click();
  await profile.getByLabel("Mot de passe actuel",{exact:true}).fill("current-test-password");
  await profile.getByLabel("Nouveau mot de passe",{exact:true}).fill("new-test-password");
  await profile.getByRole("button",{name:"Changer le mot de passe",exact:true}).click();
  await expect(page.getByRole("alert").filter({hasText:"Mot de passe actuel incorrect."})).toBeVisible();
  await expect(profile.getByLabel("Mot de passe actuel",{exact:true})).toBeVisible();
  passwordFailure = false;
  await profile.getByRole("button",{name:"Changer le mot de passe",exact:true}).click();
  await expect(page.getByRole("button",{name:"Se connecter",exact:true})).toBeVisible();
  assert.deepEqual(mutations.at(-1),{path:"/auth/password",method:"POST",body:{current_password:"current-test-password",new_password:"new-test-password"}});
  user = {...user,role:"viewer",username:"lecteur"};
  reads.length = 0;
  await page.reload();
  await expect(profile.getByLabel("Mot de passe actuel",{exact:true})).toBeVisible();
  await expect(page.getByRole("region",{name:"Administration des utilisateurs",exact:true})).toHaveCount(0);
  assert.ok(!reads.includes("/auth/users"));
  await expect(page.getByRole("link",{name:"Utilisateurs",exact:true})).toHaveCount(0);
  await page.evaluate(() => { location.hash = "account"; });
  await expect(page).toHaveURL(/#profile$/);
  await expect(page.getByRole("button",{name:"Créer un jeton",exact:true})).toBeEnabled();
  assert.ok(!reads.includes("/auth/users"));
  user = {...user,issuer:"https://identity.example.test",username:"operateur-sso"};
  await page.reload();
  await expect(profile.getByText("operateur-sso",{exact:true})).toBeVisible();
  await expect(profile.getByLabel("Mot de passe actuel",{exact:true})).toHaveCount(0);
  await expect(profile.getByText(/géré par votre fournisseur d’identité/)).toBeVisible();
  await menuTrigger.click();
  await page.getByRole("menuitem",{name:"Se déconnecter",exact:true}).click();
  await expect(page.getByRole("button",{name:"Se connecter",exact:true})).toBeVisible();
  assert.equal(mutations.at(-1).path,"/auth/logout");
  assert.deepEqual(errors,[]);
  console.log("PASS avatar menu, keyboard navigation, dedicated profile, no lab badge, responsive layout, token creation/revocation, user creation/roles, password error and logout, viewer and SSO restrictions; simulated API only");
} finally {
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
