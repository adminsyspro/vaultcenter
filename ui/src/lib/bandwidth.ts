import { translate as t } from "./i18n";
export type BandwidthStatus = {
  vtl_id: string;
  revision: number;
  tape_write_bps: number | null;
  s3_bps: number | null;
  tape: {state: "confirmed" | "pending" | "stopped" | "unavailable"; observed_revision: number | null};
  s3: {state: "configured" | "degraded"};
  message: string | null;
};
const rate = (v: unknown) => v === null || typeof v === "number" && Number.isSafeInteger(v) && v >= 1_000_000 && v <= 100_000_000_000 && v % 1_000_000 === 0;
export function bandwidthStatus(value: unknown, id: string): BandwidthStatus {
  const v = value as BandwidthStatus | null;
  if (!v || v.vtl_id !== id || !Number.isSafeInteger(v.revision) || v.revision < 1 || !rate(v.tape_write_bps) || !rate(v.s3_bps)
      || !v.tape || !["confirmed", "pending", "stopped", "unavailable"].includes(v.tape.state)
      || !v.s3 || !["configured", "degraded"].includes(v.s3.state)) throw new Error(t("Réglages incomplets ou associés à une autre VTL."));
  return v;
}
export function megabytes(value: string): number {
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number < 1 || number > 100_000) throw new Error(t("Indiquer un entier de 1 à 100 000 Mo/s."));
  return number * 1_000_000;
}
