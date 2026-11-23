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
  browser = await chromium.launch({executablePath:process.env.COLD_VTL_CHROMIUM || undefined,args:["--no-sandbox"]});
  const page = await browser.newPage({locale:'fr-FR',viewport:{width:1920,height:1100},timezoneId:"Europe/Paris"});
  await page.clock.setFixedTime(new Date("2026-09-15T12:00:00Z"));
  const errors=[];
  page.on("pageerror",error=>errors.push(error.message));
  let cartridges = ["verified","verified","writable","failed"].map((state,index)=>({barcode:`TEST00${index}L8`,state,generation:1,bytes_written:0,capacity_bytes:1024}));
  let jobs = [
    ["completed","2026-09-14 22:30:00"],
    ["failed","2026-09-15 08:00:00"],
    ["running","2026-09-15T09:00:00Z"],
    ["queued","2026-09-09T10:00:00Z"],
    ["completed","2026-08-25T10:00:00Z"],
    ["completed","2026-07-01T10:00:00Z"],
  ].map(([status,created_at],index)=>({id:`job-${index}`,kind:"export",subject:"TEST",status,created_at,updated_at:created_at,progress:0,message:null}));
  await page.route("**/auth/**",route=>route.fulfill({json:new URL(route.request().url()).pathname==="/auth/config"?{local:true,oidc:false}:{user:{id:"test",username:"lecture",role:"viewer",issuer:null,disabled:false},csrf_token:"test"}}));
  await page.route("**/v1/**",route=>{
    if (new URL(route.request().url()).pathname.endsWith("/telemetry")) return route.fulfill({json:{version:1,vtl_id:new URL(route.request().url()).pathname.split("/")[3],generated_at:null,observed_at:null,source:"mhvtl",status:"unavailable",reason:"not_configured",drives:[],slots:[],media:[]}});
    assert.equal(route.request().method(),"GET");
    const originalPath = new URL(route.request().url()).pathname;
    if (originalPath === "/v1/vtls") return route.fulfill({json:[{id:"00000000-0000-4000-8000-000000000001",name:"VTL principale",lifecycle:"ready",provisioning_revision:1}]});
    // The header bell lists every VTL, so its route is the one global read.
    if (originalPath === "/v1/activity") return route.fulfill({json:{active:[],recent:[],seen_at:null,unread:0}});
    assert.ok(originalPath.startsWith("/v1/vtls/00000000-0000-4000-8000-000000000001/"), `Unscoped request: ${originalPath}`);
    const path = originalPath.replace("/v1/vtls/00000000-0000-4000-8000-000000000001/","/v1/");
    const data = {
      "/v1/maintenance":{configured:false},
      "/v1/telemetry":{version:1,vtl_id:"00000000-0000-4000-8000-000000000001",generated_at:null,observed_at:null,source:"mhvtl",status:"unavailable",reason:"not_configured",drives:[],slots:[],media:[]},
      "/v1/status":{status:"ok",cartridges:cartridges.length,jobs:101,media_root:"/opt/mhvtl"},
      "/v1/cartridges":cartridges,
      "/v1/jobs":jobs,
      "/v1/cartridges/options":{storage:null},
    };
    if(path === "/v1/telemetry/history")return route.fulfill({json:emptyThroughputHistory("00000000-0000-4000-8000-000000000001",route.request().url())});
    assert.ok(path in data,`Unexpected API request ${path}`);
    return route.fulfill({json:data[path]});
  });
  const url=`http://127.0.0.1:${server.address().port}/#overview`;
  await page.goto(url);
  const charts=page.getByRole("region",{name:"Graphiques de la bibliothèque"});
  await expect(charts.locator(".recharts-surface")).toHaveCount(3);
  await expect(charts.getByText("Historique limité aux 100 dernières opérations.",{exact:false})).toHaveCount(0);
  await charts.getByText("Voir les valeurs par jour").click();
  const table=charts.getByRole("table");
  await expect(table.locator("tbody tr")).toHaveCount(7);
  assert.deepEqual(await table.locator("tbody tr").last().getByRole("cell").allTextContents(),["1","1","1"]);
  assert.deepEqual(await table.locator("tbody tr").first().getByRole("cell").allTextContents(),["0","1","0"]);
  const results=charts.locator('[data-slot="card"]').filter({has:page.getByText("Résultat des opérations",{exact:true})});
  await expect(results.getByRole("listitem").first()).toContainText("25%");
  await charts.getByRole("button",{name:"30 jours",exact:true}).click();
  await expect(table.locator("tbody tr")).toHaveCount(30);
  await expect(results.getByRole("listitem").first()).toContainText("40%");
  const media=charts.locator('[data-slot="card"]').filter({has:page.getByText("État des cartouches",{exact:true})});
  await expect(media.getByRole("listitem").first()).toContainText("50%");
  await charts.getByText("Voir les valeurs par jour").click();
  await page.screenshot({path:"/tmp/cold-vtl-charts-desktop.png",fullPage:true});
  await page.getByRole("button",{name:"Changer le thème"}).click();
  await page.screenshot({path:"/tmp/cold-vtl-charts-dark.png",fullPage:true});
  for(const width of [2560,1440,768,390]) {
    await page.setViewportSize({width,height:1000});
    await expect(charts.locator(".recharts-surface")).toHaveCount(3);
    await expect.poll(()=>page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  }
  await page.screenshot({path:"/tmp/cold-vtl-charts-mobile.png",fullPage:true});
  jobs=[];cartridges=[];
  await page.reload();
  await expect(charts.getByText("Aucune opération sur cette période.")).toBeVisible();
  await expect(charts.getByText("Aucune cartouche dans le catalogue.")).toBeVisible();
  await expect(charts.locator(".recharts-surface")).toHaveCount(2);
  assert.deepEqual(errors,[]);
  console.log("PASS dashboard: periods, UTC/local date boundaries, status totals, percentages, empty data, viewer access, desktop/mobile, dark theme, no mutations or JavaScript errors");
} finally {
  await browser?.close();
  await new Promise(resolve=>server.close(resolve));
}
