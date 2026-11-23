/** The local volume as the cartridge controller sees it before a creation. */
export interface StorageGauge {
  total: number;
  /** Written on the volume, whatever wrote it. */
  occupied: number;
  /** Still promised to existing cartridges, within the free space. */
  reserved: number;
  /** What a new cartridge or batch may use. */
  available: number;
  /** Promises the free space cannot honour. */
  shortfall: number;
  level: "ok" | "low" | "empty";
}

const LOW = 0.15;
const bytes = (value: unknown): value is number => typeof value === "number" && Number.isFinite(value) && value >= 0;

/** Reads the `storage` budget of the cartridge options; null when it is missing or inconsistent. */
export function storageGauge(budget: unknown): StorageGauge | null {
  if (!budget || typeof budget !== "object") return null;
  const { total_bytes: total, free_bytes: free, reserved_bytes: promised } = budget as Record<string, unknown>;
  if (!bytes(total) || !bytes(free) || !bytes(promised) || total === 0 || free > total) return null;
  const reserved = Math.min(promised, free);
  const available = free - reserved;
  return {
    total, occupied: total - free, reserved, available, shortfall: promised - reserved,
    level: available <= 0 ? "empty" : available / total < LOW ? "low" : "ok",
  };
}

/** Bar widths in whole percents that add up to 100. */
export function gaugeWidths(gauge: StorageGauge) {
  const occupied = Math.round(gauge.occupied / gauge.total * 100);
  const reserved = Math.min(100 - occupied, Math.round(gauge.reserved / gauge.total * 100));
  return { occupied, reserved, available: 100 - occupied - reserved };
}
