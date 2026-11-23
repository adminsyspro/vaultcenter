"use client";
import { useEffect, useState } from "react";
import { Videotape } from "lucide-react";
import { translate as t, useI18n } from "@/lib/i18n";
import { request } from "@/lib/api";
import { clientBinding } from "@/lib/client-binding";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { SectionNav } from "./section-nav";
import { VtlSetup } from "./vtl-setup";
import { TapeStorage } from "./tape-storage";
import { VtlS3Storage } from "./vtl-s3-storage";
import { PbsConnectionPanel } from "./pbs-connection";
import { BandwidthSettings } from "./bandwidth";

export type VtlTab = "setup" | "storage" | "connection" | "bandwidth";
export type StorageTab = "local" | "s3";
export const vtlTabs: VtlTab[] = ["setup", "storage", "connection", "bandwidth"];
type Library = { id: string; name: string; lifecycle: string; provisioning_revision: number; storage?: { bucket: string } | null };
const lifecycleLabels: Record<string, string> = { draft: "À configurer", ready: "Prête", failed: "À vérifier", disabled: "Désactivée" };

/** Reads the client type of the VTL for the labels; until it is known the wording stays neutral. */
function VtlBandwidth({ vtlId }: { vtlId: string }) {
  const [source, setSource] = useState<string | undefined>(undefined);
  useEffect(() => {
    const controller = new AbortController();
    void request<unknown>(`/v1/vtls/${encodeURIComponent(vtlId)}/client`, { signal: controller.signal })
      .then(value => {
        if (controller.signal.aborted) return;
        const kind = clientBinding(value, vtlId).client;
        setSource(kind === "pbs" ? "PBS" : kind === "veeam" ? "Veeam" : undefined);
      })
      .catch(cause => { if (!controller.signal.aborted) { console.error("VTL client read failed", cause); setSource(undefined); } });
    return () => controller.abort();
  }, [vtlId]);
  return <BandwidthSettings vtlId={vtlId} source={source} />;
}

/** The single window of a VTL. Every prerequisite is set inside it: no window opens above another. */
export function VtlWindow({ vtl, tab, storage, canManage, connected, onTab, onStorage, onClose, onRefresh }: {
  vtl: Library; tab: VtlTab; storage: StorageTab; canManage: boolean; connected: boolean;
  onTab: (tab: VtlTab) => void; onStorage: (tab: StorageTab) => void; onClose: () => void; onRefresh: () => Promise<void>;
}) {
  useI18n();
  return <Dialog open onOpenChange={open => { if (!open) onClose(); }}>
    <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-3xl">
      <DialogHeader icon={Videotape}><DialogTitle>{vtl.name}</DialogTitle><DialogDescription>{t(lifecycleLabels[vtl.lifecycle] ?? "État inconnu")}</DialogDescription></DialogHeader>
      <SectionNav label={t("Rubriques de la VTL")} value={tab} onChange={onTab} items={[
        { value: "setup", label: t("Mise en service") }, { value: "storage", label: t("Stockage") },
        { value: "connection", label: t("Connexion") }, { value: "bandwidth", label: t("Débit") },
      ]} />
      {tab === "setup" && <VtlSetup key={vtl.id} vtl={vtl} onEditStorage={() => { onStorage("local"); onTab("storage"); }} />}
      {tab === "storage" && <div className="space-y-5">
        <SectionNav label={t("Type de stockage")} value={storage} onChange={onStorage} items={[{ value: "local", label: t("Local") }, { value: "s3", label: t("S3") }]} />
        {storage === "local"
          ? <TapeStorage key={vtl.id} vtlId={vtl.id} />
          : <VtlS3Storage key={vtl.id} vtlId={vtl.id} activeBucket={vtl.storage?.bucket ?? null} canManage={canManage} connected={connected} onRefresh={onRefresh} />}
      </div>}
      {tab === "connection" && <PbsConnectionPanel key={vtl.id} vtlId={vtl.id} />}
      {tab === "bandwidth" && <VtlBandwidth key={vtl.id} vtlId={vtl.id} />}
    </DialogContent>
  </Dialog>;
}
