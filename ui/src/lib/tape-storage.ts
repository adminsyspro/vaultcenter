import { translate as t, getLocale } from "./i18n";
export interface TapeStorageStatus {
  vtl_id:string;
  data_locked:boolean;
  change_allowed:boolean;
  change_lock_reason:string|null;
  mode: "legacy" | "managed";
  phase: "unconfigured" | "ready" | "applying" | "unavailable" | "recovery_required";
  revision: number;
  path: string | null;
  capacity: {total_bytes:number;available_bytes:number} | null;
  operation: {id:string;phase:string} | null;
  message: string | null;
}
export interface CheckedPath {vtl_id:string;path:string;filesystem:string;total_bytes:number;available_bytes:number;check_id:string}
export interface StorageApplication {path:string;revision:number;check_id:string;request_id:string}
export function storageStatus(value:unknown,vtlId:string): TapeStorageStatus {
  if (!value || typeof value!=="object") throw new Error(t("État du stockage invalide."));
  const v=value as TapeStorageStatus;
  if (v.vtl_id!==vtlId || typeof v.data_locked!=="boolean" || typeof v.change_allowed!=="boolean" || (v.data_locked && v.change_allowed)) throw new Error(t("État du stockage associé à une autre VTL ou incomplet."));
  if (!["legacy","managed"].includes(v.mode) || !["unconfigured","ready","applying","unavailable","recovery_required"].includes(v.phase) || !Number.isSafeInteger(v.revision)) throw new Error(t("État du stockage invalide."));
  return v;
}
export function space(bytes:number) {return new Intl.NumberFormat(getLocale(),{maximumFractionDigits:1}).format(bytes/1024**3)+" "+t("Gio");}
