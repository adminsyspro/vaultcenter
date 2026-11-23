"use client";
import {translate as t,useI18n,getLocale} from "@/lib/i18n";

import { useCallback, useEffect, useRef, useState } from "react";
import { readSnapshot, type Snapshot } from "@/lib/api";

export function useConsole(vtlId: string | null) {
  const [data, setData] = useState<Snapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [updatedAt, setUpdatedAt] = useState<Date | null>(null);
  const mounted = useRef(true);
  const active = useRef<AbortController | null>(null);

  const refresh = useCallback(async () => {
    if (!vtlId || !mounted.current) return;
    active.current?.abort();
    const controller = new AbortController();
    active.current = controller;
    setRefreshing(true);
    try {
      const snapshot = await readSnapshot(
        AbortSignal.any([controller.signal, AbortSignal.timeout(10000)]),
        vtlId,
      );
      if (controller.signal.aborted) return;
      setData(snapshot);
      setError(null);
      setUpdatedAt(new Date());
    } catch (cause) {
      if (!controller.signal.aborted)
        setError(
          cause instanceof Error ? cause.message : t("Connexion impossible."),
        );
    } finally {
      if (active.current === controller) {
        active.current = null;
        setRefreshing(false);
      }
    }
  }, [vtlId]);

  useEffect(() => {
    mounted.current = true;
    void refresh();
    const timer = setInterval(() => {
      if (!document.hidden && !active.current) void refresh();
    }, 5000);
    const onVisible = () => {
      if (!document.hidden && !active.current) void refresh();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      mounted.current = false;
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
      active.current?.abort();
    };
  }, [refresh]);

  return { data, error, refreshing, updatedAt, refresh };
}
