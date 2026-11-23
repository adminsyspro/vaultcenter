import { translate as t } from "./i18n";
export type PbsConnectionInfo = {
  vtl_id: string; revision: number; client: "pbs" | "veeam"; phase: "ready" | "missing_prerequisite";
  pbs: string | null; host: string | null; port: number; message: string | null;
  steps: {id: string; title: string; explanation: string; command: string}[];
};
export function connectionInfo(value: unknown, id: string): PbsConnectionInfo {
  const v = value as PbsConnectionInfo | null;
  if (!v || v.vtl_id !== id || !Number.isSafeInteger(v.revision) || v.revision < 0
      || !["pbs", "veeam"].includes(v.client) || !["ready", "missing_prerequisite"].includes(v.phase) || !Number.isInteger(v.port)
      || !Array.isArray(v.steps) || !v.steps.every(s => typeof s.id === "string" && typeof s.title === "string" && typeof s.command === "string")) throw new Error(t("Procédure PBS invalide ou associée à une autre VTL."));
  return v;
}
