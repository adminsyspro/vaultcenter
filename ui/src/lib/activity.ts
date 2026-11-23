import {kindLabels} from "./api";

export type ActivityOutcome = "success" | "failure" | "uncertain";
export type ActivityItem = {
  id: string;
  source: "job" | "provider";
  kind: string;
  vtl_id: string;
  vtl_name: string;
  subject: string;
  destination_id: string | null;
  started_at?: string;
  progress?: number | null;
  outcome?: ActivityOutcome;
  finished_at?: string;
  unread?: boolean;
};
export type ActivityFeed = {
  active: ActivityItem[];
  recent: ActivityItem[];
  seen_at: string | null;
  unread: number;
};

export function isActivityFeed(value: unknown): value is ActivityFeed {
  const feed = value as Partial<ActivityFeed> | null;
  return !!feed && Array.isArray(feed.active) && Array.isArray(feed.recent) && typeof feed.unread === "number";
}

const providerLabels: Record<string, Record<ActivityOutcome | "active", string>> = {
  archive: {active: "Archivage chez OVH", success: "Archivé chez OVH", uncertain: "Archivage incertain chez OVH", failure: "Archivage refusé par OVH"},
  recall: {active: "Rappel chez OVH", success: "Rappel terminé chez OVH", uncertain: "Rappel incertain chez OVH", failure: "Rappel refusé par OVH"},
};

/** French label, translated by the caller. */
export function activityLabel(item: ActivityItem): string {
  if (item.source === "job") return kindLabels[item.kind] ?? "Opération en arrière-plan";
  return providerLabels[item.kind]?.[item.outcome ?? "active"] ?? "Opération chez OVH";
}

/** OVH requests live in Stockage S3, jobs in Activité. */
export function activityHref(item: ActivityItem): string {
  return `#${item.source === "provider" ? "storage" : "jobs"}?vtl=${encodeURIComponent(item.vtl_id)}`;
}
