import { type Cartridge, type Job, isActive } from "./api";
import { type Telemetry, occupancy } from "./telemetry";

/** Cartridge states folded into the four families the fleet charts show. */
export type Family = "writable" | "transfer" | "protected" | "failed";
export const families: Family[] = ["writable", "transfer", "protected", "failed"];
export const familyLabels: Record<Family, string> = {
  writable: "Inscriptible",
  transfer: "En transfert",
  protected: "Protégée S3",
  failed: "Échec",
};
const familyOf: Record<string, Family> = {
  writable: "writable",
  sealed: "transfer", uploading: "transfer", archiving: "transfer", restoring: "transfer", hydrating: "transfer",
  verified: "protected", archived: "protected", online: "protected",
  failed: "failed",
};
export const family = (state: string): Family => familyOf[state] ?? "writable";

export type VtlSnapshot = {
  id: string; name: string; lifecycle: string;
  cartridges: Cartridge[] | null; telemetry: Telemetry | null; jobs: Job[] | null;
  disk: { total_bytes: number; available_bytes: number } | null;
  failed: boolean;
};
export type Tally = Record<Family, { count: number; bytes: number }>;
const emptyTally = (): Tally => ({ writable: { count: 0, bytes: 0 }, transfer: { count: 0, bytes: 0 }, protected: { count: 0, bytes: 0 }, failed: { count: 0, bytes: 0 } });

/** Real usage: drive/MAM measurement or local file size, like the library page. */
export function usedBytes(cartridge: Cartridge, telemetry: Telemetry | null) {
  return occupancy(cartridge, telemetry?.media.find(m => m.barcode === cartridge.barcode)).used;
}

export function tally(vtl: VtlSnapshot): Tally {
  const result = emptyTally();
  for (const cartridge of vtl.cartridges ?? []) {
    if (cartridge.local_deleted) continue;
    const bucket = result[family(cartridge.state)];
    bucket.count++; bucket.bytes += usedBytes(cartridge, vtl.telemetry);
  }
  return result;
}

export function summarize(vtls: VtlSnapshot[]) {
  const total = emptyTally();
  let capacity = 0, diskTotal = 0, diskAvailable = 0;
  const disks = new Set<string>();
  for (const vtl of vtls) {
    const t = tally(vtl);
    for (const f of families) { total[f].count += t[f].count; total[f].bytes += t[f].bytes; }
    capacity += (vtl.cartridges ?? []).filter(c => !c.local_deleted).reduce((sum, c) => sum + c.capacity_bytes, 0);
    // VTLs often share one volume: count each distinct (total, available) pair once.
    if (vtl.disk) { const key = `${vtl.disk.total_bytes}:${vtl.disk.available_bytes}`; if (!disks.has(key)) { disks.add(key); diskTotal += vtl.disk.total_bytes; diskAvailable += vtl.disk.available_bytes; } }
  }
  const count = families.reduce((sum, f) => sum + total[f].count, 0);
  const written = families.reduce((sum, f) => sum + total[f].bytes, 0);
  return { total, count, written, capacity, protectedShare: written ? total.protected.bytes / written : null,
    disk: diskTotal ? { total: diskTotal, used: diskTotal - diskAvailable } : null };
}

export function activeDrives(vtl: VtlSnapshot) {
  return (vtl.telemetry?.status === "available" ? vtl.telemetry.drives : []).filter(d => ["writing", "reading", "loading", "unloading"].includes(d.state));
}
export function activeJobs(vtl: VtlSnapshot) { return (vtl.jobs ?? []).filter(isActive); }
