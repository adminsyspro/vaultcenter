export function versionLabel(status: unknown): string | null {
  if (typeof status !== "object" || status === null) return null;
  const { version, revision } = status as { version?: unknown; revision?: unknown };
  if (typeof version !== "string" || version === "") return null;
  const short = typeof revision === "string" && /^[0-9a-f]{7,40}$/i.test(revision) ? ` · ${revision.slice(0, 7)}` : "";
  return `VaultCenter ${version}${short}`;
}
