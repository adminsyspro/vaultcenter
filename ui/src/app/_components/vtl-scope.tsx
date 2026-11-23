"use client";
import {translate as t,useI18n,getLocale} from "@/lib/i18n";
import { createContext, useCallback, useContext, useEffect, useRef } from "react";
import { request } from "@/lib/api";
export const VtlScope = createContext<string | null>(null);
export function scopedPath(path: string, id: string) {
  return path.startsWith("/v1/") && path !== "/v1/vtls" && !path.startsWith("/v1/vtls/")
    ? `/v1/vtls/${encodeURIComponent(id)}${path.slice(3)}` : path;
}
export function useScopedRequest() {
  const id = useContext(VtlScope);
  const controllers = useRef(new Set<AbortController>());
  useEffect(() => {
    const pending = controllers.current;
    return () => { for (const controller of pending) controller.abort(); pending.clear(); };
  }, [id]);
  return useCallback(async function scopedRequest<T>(path: string, options: RequestInit = {}): Promise<T> {
    if (!id) throw new Error(t("Sélectionnez une bibliothèque VTL."));
    const controller = new AbortController();
    controllers.current.add(controller);
    try {
      const signal = AbortSignal.any([controller.signal, options.signal ?? AbortSignal.timeout(15000)]);
      return await request<T>(scopedPath(path,id), { ...options, signal });
    } finally { controllers.current.delete(controller); }
  }, [id]);
}
