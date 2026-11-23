"use client";
import {translate as t,useI18n,getLocale} from "@/lib/i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { request } from "@/lib/api";
export type Library = { id: string; name: string; lifecycle: string; storage?: {provider: string; bucket: string; prefix: string} | null };
export function vtlHash(view: string, id: string | null) {
  return `#${view}${id ? `?vtl=${encodeURIComponent(id)}` : ""}`;
}
// Opens the library's window on a tab (handled by the VTLs page).
export function vtlWindowHash(id: string, tab: "setup" | "storage" | "connection" | "bandwidth", storage?: "local" | "s3") {
  return `#vtls?vtl=${encodeURIComponent(id)}&tab=${tab}${storage ? `&storage=${storage}` : ""}`;
}
export function vtlStorageHash(id: string) {
  return vtlWindowHash(id, "storage", "s3");
}
export function useVtlSelection() {
  const [libraries, setLibraries] = useState<Library[]>([]);
  const [id, setId] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState("");
  const lastId = useRef<string | null>(null);
  useEffect(() => {
    function read() {
      const parts = location.hash.slice(1).split("?");
      const query = new URLSearchParams(parts[1] ?? "");
      const next = query.has("vtl") ? query.get("vtl") : lastId.current;
      if (next) lastId.current = next;
      setId(next);
      if (!query.has("vtl") && next) history.replaceState(null,"",vtlHash(parts[0] || "media",next));
    }
    read();window.addEventListener("hashchange",read);
    return () => window.removeEventListener("hashchange",read);
  }, []);
  useEffect(() => {
    const controller = new AbortController();
    let pending = false;
    async function load() {
      if (pending || controller.signal.aborted) return;
      pending = true;
      try {
        const values = await request<Library[]>("/v1/vtls", {signal:AbortSignal.any([controller.signal,AbortSignal.timeout(15000)])});
        if (!Array.isArray(values) || values.some(v => typeof v.id !== "string" || typeof v.name !== "string" || typeof v.lifecycle !== "string")) throw new Error(t("Liste des bibliothèques invalide."));
        if (controller.signal.aborted) return;
        setLibraries(values);setLoaded(true);setError("");
        if (lastId.current === null && !new URLSearchParams(location.hash.split("?")[1] ?? "").has("vtl")) {
          const selected = values.find(v => v.lifecycle === "ready") ?? values[0];
          if (selected) {
            lastId.current=selected.id;setId(selected.id);
            history.replaceState(null,"",vtlHash(location.hash.slice(1).split("?")[0] || "media",selected.id));
          }
        }
      } catch (cause) { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : t("Bibliothèques indisponibles.")); }
      finally { pending=false; }
    }
    void load();
    const timer=setInterval(() => { if (!document.hidden) void load(); },15000);
    const refresh=()=>void load();
    window.addEventListener("cold-vtl-libraries-changed",refresh);
    window.addEventListener("focus",refresh);
    return () => { controller.abort(); clearInterval(timer);window.removeEventListener("cold-vtl-libraries-changed",refresh);window.removeEventListener("focus",refresh); };
  }, []);
  const selected = libraries.find(v => v.id === id) ?? null;
  const unknown = loaded && id !== null && !selected;
  const select = useCallback((next: string) => {
    location.hash = vtlHash(location.hash.slice(1).split("?")[0] || "media",next);
  }, []);
  return { libraries, selected, id: selected?.id ?? null, loaded, error, unknown, select };
}
