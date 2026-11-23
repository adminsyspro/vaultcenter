import * as React from "react";
import { Input } from "@/components/ui/input";
import { cn } from "cn";

/** Mirrors cold_vtl_core::MANAGED_BUCKET_PREFIX: every bucket VaultCenter creates carries it. */
export const MANAGED_BUCKET_PREFIX = "vc-";

export const managedBucketSuffix = (name: string) => name.startsWith(MANAGED_BUCKET_PREFIX) ? name.slice(MANAGED_BUCKET_PREFIX.length) : name;

export const validManagedBucketSuffix = (suffix: string) => /^[a-z0-9-]{0,59}[a-z0-9]$/.test(suffix);

/** Edits only the part after the fixed prefix; callers prepend MANAGED_BUCKET_PREFIX before sending. */
export function ManagedBucketInput({ className, action, ...props }: Omit<React.ComponentProps<"input">, "maxLength" | "pattern"> & { action?: React.ReactNode }) {
  return (
    <div className={cn("relative flex", className)}>
      <span aria-hidden="true" className="inline-flex h-8 items-center rounded-l-lg border border-r-0 border-input bg-muted px-2.5 font-mono text-sm text-muted-foreground">{MANAGED_BUCKET_PREFIX}</span>
      <Input {...props} className={cn("rounded-l-none font-mono", action && "pr-28")} autoComplete="off" spellCheck={false} maxLength={63 - MANAGED_BUCKET_PREFIX.length} pattern="[a-z0-9\-]{0,59}[a-z0-9]" />
      {action}
    </div>
  );
}
