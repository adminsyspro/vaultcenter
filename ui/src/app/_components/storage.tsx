"use client";
import { translate as t, useI18n } from "@/lib/i18n";

import { useScopedRequest } from "./vtl-scope";
import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Alert, AlertTitle, AlertDescription } from "@/components/ui/alert";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogClose, DialogFooter } from "@/components/ui/dialog";
import {type BucketInventory} from './storage-cartridges';
import { StorageRecovery } from './storage-recovery';
import { StorageHistory } from './storage-history';
import { SectionNav } from './section-nav';
import { BucketState } from "./archive";
import { vtlStorageHash } from "./use-vtl-selection";
import { bucketPath } from "./bucket-selector";
import { type Job } from "@/lib/api";

interface StorageReport {
  destination_id: string;
  active: boolean;
  managed?: boolean;
  next_bucket?: string | null;
  bucket: string;
  prefix: string;
  region: string;
  provider: string;
  provider_label?: string;
  supported: boolean;
  raw_status: string | null;
  state_label: string;
  object_access: string;
  archive_lock_until: string | null;
  multipart_uploads: number | null;
  checked_at: number;
  demo?: boolean;
  inventory: BucketInventory | null;
  checks: { id: string; label: string; level: string; detail: string }[];
  catalogue_generations: {
    barcode: string;
    generation: number;
    key: string;
    checked_at: string;
  }[];
}
type StorageProps = {canManage: boolean; jobs: Job[]; onRefresh: () => Promise<void>; connected: boolean};
// Consultation and operations only: S3 setup lives in the VTL "Stockage" dialog (S3 tab).
export function Storage({vtlId, vtlName, ready, needsConfiguration = false, ...props}: StorageProps & {vtlId: string; vtlName: string; ready: boolean; needsConfiguration?: boolean}) {
  useI18n();
  const request = useScopedRequest();
  const [connection, setConnection] = useState<boolean | null>(null);
  useEffect(() => {
    setConnection(null);
    if (!needsConfiguration || !props.canManage) return;
    const controller = new AbortController();
    void request<{configured: boolean}>("/v1/storage/connection", {signal: controller.signal})
      .then(value => { if (!controller.signal.aborted) setConnection(value.configured === true); }).catch(() => {});
    return () => controller.abort();
  }, [needsConfiguration, props.canManage, request]);
  const configure = () => { location.hash = vtlStorageHash(vtlId); };
  return <div className="space-y-5">
    {needsConfiguration && <Alert>
      <AlertTitle>{t(connection ? "Aucune destination S3 active" : "Stockage S3 non configuré")}</AlertTitle>
      <AlertDescription>{props.canManage
        ? <div className="mt-2 space-y-3"><p>{t(connection ? "La connexion est enregistrée. Créez une destination pour envoyer les cartouches vers S3." : "La connexion et la destination S3 se configurent depuis le stockage de la bibliothèque.")}</p><Button size="sm" onClick={configure}>{connection ? t("Créez la première destination de {name}", {name: vtlName}) : t("Configurer le stockage de {name}", {name: vtlName})}</Button></div>
        : t("Un administrateur doit configurer la connexion S3 de cette bibliothèque.")}</AlertDescription>
    </Alert>}
    {ready ? <StorageInventory {...props} /> : <Alert>
      <AlertTitle>{t("Bibliothèque en préparation")}</AlertTitle>
      <AlertDescription>{t("Les opérations seront disponibles après activation de cette VTL.")}{props.canManage && <Button variant="link" onClick={configure}>{t("Configurer son stockage S3")}</Button>}
      </AlertDescription>
    </Alert>}
  </div>;
}
function StorageInventory(props: StorageProps) {
  useI18n();
  const [bucket, setBucket] = useState<{ id: string; name: string; createdAt?: string } | null>(null);
  const [section, setSection] = useState<"copies" | "history">("copies");
  return <>
    <SectionNav label={t("Rubriques de Stockage S3")} value={section} onChange={setSection}
      items={[{ value: "copies", label: t("Buckets et copies") }, { value: "history", label: t("Historique") }]} />
    {section === "copies"
      ? <StorageRecovery {...props} onBucket={(id, name, createdAt) => setBucket({ id, name, createdAt })} />
      : <StorageHistory jobs={props.jobs} onBucket={(id, name) => setBucket({ id, name })} />}
    <Dialog open={bucket !== null} onOpenChange={open => { if (!open) setBucket(null); }}>
      <DialogContent className="max-h-[85dvh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle className="break-all">{bucket?.name}</DialogTitle>
          <DialogDescription className="sr-only">{t("État du bucket et opérations OVH.")}</DialogDescription>
        </DialogHeader>
        {bucket && <StorageBucketView key={bucket.id} {...props} destinationId={bucket.id} bucketName={bucket.name} createdAt={bucket.createdAt} />}
        <DialogFooter><DialogClose asChild><Button variant="outline">{t("Fermer")}</Button></DialogClose></DialogFooter>
      </DialogContent>
    </Dialog>
  </>;
}
function StorageBucketView({canManage,jobs,onRefresh,connected,destinationId,bucketName,createdAt}: StorageProps & {destinationId:string; bucketName:string; createdAt?:string}) {
  useI18n();
  const request = useScopedRequest();
  const [managedArchive, setManagedArchive] = useState(false);
  const [data, setData] = useState<StorageReport | null>(null);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  const sequence = useRef(0);
  const refresh = useCallback(async (signal?: AbortSignal) => {
    const revision = ++sequence.current;
    setBusy(true);
    setError("");
    setData(null);
    try {
      const value = await request<StorageReport>(bucketPath("/v1/storage", destinationId), {
        signal: signal
          ? AbortSignal.any([signal, AbortSignal.timeout(45000)])
          : AbortSignal.timeout(45000),
      });
      if (!signal?.aborted && revision === sequence.current) {
        setData(value); setManagedArchive(value.supported);
      }
    } catch (e) {
      if (!signal?.aborted && revision === sequence.current)
        setError(
          e instanceof Error ? e.message : t("Lecture du stockage impossible."),
        );
    } finally {
      if (!signal?.aborted && revision === sequence.current) setBusy(false);
    }
  }, [request, destinationId]);
  useEffect(() => {
    const controller = new AbortController();
    void refresh(controller.signal);
    return () => controller.abort();
  }, [refresh]);
  const cloudCompletion = jobs.filter(j => /^(export|reconcile|(archive|recall)_(submit|refresh))$/.test(j.kind) && !["queued","running"].includes(j.status)).map(j => j.id + j.status).join(",");
  useEffect(() => { if (cloudCompletion) void refresh(); }, [cloudCompletion, refresh]);
  useEffect(() => {
    if (!["Archiving","Restoring"].includes(data?.raw_status ?? "")) return;
    const timer = setInterval(() => { if (!document.hidden) void refresh(); }, 30000);
    return () => clearInterval(timer);
  }, [data?.raw_status,refresh]);
  return (
    <div className="space-y-5">
      {busy && (
        <p role="status">{t("Lecture de l’état et de l’inventaire du stockage…")}</p>
      )}
      {error && (
        <Alert variant="destructive">
          <AlertTitle>{t("État du stockage non confirmé")}</AlertTitle>
          <AlertDescription>
            {t(error)} {t("Rechargez la page pour réessayer.")}</AlertDescription>
        </Alert>
      )}
      {data && !data.supported && <Alert><AlertTitle>{t("Archivage géré indisponible")}</AlertTitle><AlertDescription>{t("Ce fournisseur permet les accès S3 aux objets disponibles. L’archivage Glacier et le rappel ne sont pas encore intégrés.")}</AlertDescription></Alert>}
      {managedArchive && data && <BucketState key={destinationId} destinationId={destinationId} bucketName={bucketName} createdAt={createdAt} canManage={canManage} jobs={jobs} onRefresh={onRefresh} onStorageRefresh={refresh} storage={data} connected={connected} />}

    </div>
  );
}
