"use client";
import {useI18n} from '@/lib/i18n';
import {
  CheckCircle2,
  Circle,
  Clock3,
  LoaderCircle,
  LockKeyhole,
  XCircle,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { jobLabels, mediaLabels } from "@/lib/api";

export function StatusBadge({
  state,
  job = false,
}: {
  state: string;
  job?: boolean;
}) {
 const {t}=useI18n();
  const Icon =
    state === "failed"
      ? XCircle
      : ["verified", "completed", "online"].includes(state)
        ? CheckCircle2
        : [
              "uploading",
              "running",
              "restoring",
              "hydrating",
              "archiving",
            ].includes(state)
          ? LoaderCircle
          : state === "sealed"
            ? LockKeyhole
            : state === "queued"
              ? Clock3
              : Circle;
  return (
    <Badge
      variant={
        state === "failed"
          ? "destructive"
          : ["verified", "completed"].includes(state)
            ? "default"
            : "outline"
      }
    >
      <Icon aria-hidden="true" />
      {t((job ? jobLabels : mediaLabels)[state] ?? state)}
    </Badge>
  );
}
