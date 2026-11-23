import { translate as t } from "./i18n";
export type ClientBindingInfo = {
  vtl_id: string; revision: number; client: "pbs" | "veeam" | null;
  initiator: string | null; editable: boolean; message: string | null;
};
export function clientBinding(value: unknown, id: string): ClientBindingInfo {
  const v = value as ClientBindingInfo | null;
  if (!v || v.vtl_id !== id || !Number.isSafeInteger(v.revision) || v.revision < 0
      || ![null, "pbs", "veeam"].includes(v.client) || typeof v.editable !== "boolean")
    throw new Error(t("Association du client invalide ou associée à une autre VTL."));
  return v;
}
