"use client";
import { translate as t, useI18n } from "@/lib/i18n";

import { useScopedRequest } from "./vtl-scope";
import { useEffect, useRef, useState } from "react";
import { localizedToast as toast } from "./providers";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectOption,
} from "@/components/ui/select";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { isActive, type Job, formatDate } from "@/lib/api";

interface Operation {
  id: string;
  barcode: string;
  generation: number;
  phase: string;
  created_at: number;
  backup?: string;
  original: { generation: number } | null;
  new_media?: boolean;
}
interface Overview {
  configured: boolean;
  pending: { id: string } | null;
  operations: Operation[];
}
const phases: Record<string, string> = {
  preparing: "Préparation",
  copying: "Vérification et copie",
  replacing: "Installation",
  installed: "Cartouche installée",
  resuming: "Vérification PBS",
  completed: "Disponible dans PBS",
  rolling_back: "Retour à la cartouche précédente",
  rolled_back: "Cartouche précédente rétablie",
};
export function isRestoredCopy(job: Job, barcode?: string) {
    if (
      job.subject !== barcode ||
      job.kind !== "restore" ||
      job.status !== "completed"
    )
      return false;
    try {
      const receipt = JSON.parse(job.message || "{}");
      return (
        receipt.barcode === barcode &&
        receipt.generation === job.generation &&
        Array.isArray(receipt.files) &&
        receipt.files.length > 0
      );
    } catch {
      return false;
    }
}
export function Reintegration({
  barcode,
  jobs,
  canManage,
  connected = true,
  onRefresh,
  onRestore,
}: {
  barcode?: string;
  jobs: Job[];
  canManage: boolean;
  connected?: boolean;
  onRefresh: () => Promise<void>;
  onRestore?: () => void;
}) {
  useI18n();
  const request = useScopedRequest();
  const [overview, setOverview] = useState<Overview | null>(null);
  const [selected, setSelected] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const submitting = useRef(false);
  const [confirmation, setConfirmation] = useState<{
    action: "install" | "rollback";
    id: string;
  } | null>(null);
  const revision = jobs.map((j) => `${j.id}:${j.status}`).join(",");
  useEffect(() => {
    const controller = new AbortController();
    let inFlight = false;
    const load = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        setOverview(
          await request<Overview>("/v1/reintegrations", {
            signal: controller.signal,
          }),
        );
      } catch (e) {
        if (!controller.signal.aborted) {
          setOverview(null);
          setError(e instanceof Error ? e.message : t("Suivi indisponible."));
        }
      } finally {
        inFlight = false;
      }
    };
    void load();
    const timer = setInterval(() => {
      if (!document.hidden) void load();
    }, 5000);
    return () => {
      controller.abort();
      clearInterval(timer);
    };
  }, [revision]);
  const eligible = jobs.filter(job => isRestoredCopy(job, barcode));
  const restore = eligible.find((j) => j.id === selected) ?? eligible[0];
  const operations =
    overview?.operations.filter((o) => !barcode || o.barcode === barcode) ?? [];
  const locked =
    !connected ||
    !canManage ||
    pending ||
    jobs.some(isActive) ||
    !overview?.configured;
  async function submit() {
    if (!confirmation || locked || submitting.current) return;
    submitting.current = true;
    setPending(true);
    setError("");
    try {
      const path =
        confirmation.action === "install"
          ? `/v1/cartridges/${encodeURIComponent(barcode!)}/reintegrate`
          : `/v1/reintegrations/${confirmation.id}/rollback`;
      await request<Job>(path, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(
          confirmation.action === "install"
            ? { restore_job_id: confirmation.id, replace: true }
            : {},
        ),
      });
      setConfirmation(null);
      toast.success(t("Opération lancée. Suivez sa progression dans l’activité."));
      await onRefresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : t("Opération non confirmée."));
      await onRefresh();
    } finally {
      submitting.current = false;
      setPending(false);
    }
  }
  return (
    <section
      className="flex flex-col gap-3 rounded-lg border p-4"
      aria-label={t("Réintégration PBS")}
    >
      <div>
        <h3 className="font-medium">{t("Remettre une cartouche à disposition de PBS")}</h3>
        <p className="text-sm text-muted-foreground">{t("La VTL réserve le lecteur PBS, vérifie les fichiers, conserve la cartouche actuelle puis installe la copie et reprend les services. Le lecteur doit être vide et les tâches terminées.")}</p>
      </div>
      {barcode && (
        <>
          {eligible.length ? (
            <>
              <Label htmlFor="reintegrate-restore">{t("Copie restaurée à installer")}</Label>
              <Select
                id="reintegrate-restore"
                value={restore?.id ?? ""}
                onValueChange={(selectedValue) => {
                  setSelected(selectedValue);
                  setConfirmation(null);
                }}
                disabled={locked || !!overview?.pending}
              >
                {eligible.map((j) => (
                  <SelectOption key={j.id} value={j.id}>
                    Version {j.generation} · {formatDate(j.created_at)} ·{" "}
                    {j.id.slice(0, 8)}
                  </SelectOption>
                ))}
              </Select>
              <Button
                disabled={locked || !restore || !!overview?.pending}
                onClick={() =>
                  setConfirmation({ action: "install", id: restore!.id })
                }
              >{t("Réintégrer dans PBS")}</Button>
            </>
          ) : (
            <div className="space-y-3">
              <p className="text-sm text-muted-foreground">{t("Aucune copie restaurée prête à installer. Restaurez d’abord une version depuis S3.")}</p>
              {onRestore && <Button variant="outline" onClick={onRestore}>{t("Restaurer une copie")}</Button>}
            </div>
          )}
        </>
      )}
      {overview?.pending && (
        <Alert>
          <AlertTitle>{t("Réintégration à suivre")}</AlertTitle>
          <AlertDescription>{t("Une opération est en cours ou a été interrompue. Si son job a échoué, utilisez le retour à la cartouche précédente ci-dessous. La reprise manuelle des services est bloquée jusqu’à résolution.")}</AlertDescription>
        </Alert>
      )}
      {operations.map((operation, index) => (
        <div
          key={operation.id}
          className="flex flex-col gap-2 border-t pt-3 text-sm"
        >
          <p>
            <strong>
              {operation.barcode} · Version {operation.generation}
            </strong>
            <br />
            {t(phases[operation.phase] ?? operation.phase)}
          </p>
          {operation.backup && (
            <p className="break-all text-xs text-muted-foreground">{t("Copie précédente :")} {operation.backup}
            </p>
          )}
          {(operation.phase !== "rolled_back" ||
            overview?.pending?.id === operation.id) &&
            !operations
              .slice(0, index)
              .some(
                (o) =>
                  o.barcode === operation.barcode && o.phase !== "rolled_back",
              ) && (
              <Button
                variant="outline"
                disabled={
                  locked ||
                  (!!overview?.pending && overview.pending.id !== operation.id)
                }
                onClick={() =>
                  setConfirmation({ action: "rollback", id: operation.id })
                }
              >{t(operation.new_media ? "Annuler cette réintégration ·" : "Revenir à la cartouche précédente ·")} {operation.barcode}
              </Button>
            )}
        </div>
      ))}
      {confirmation && (
        <Alert>
          <AlertTitle>{confirmation.action === "install" ? t("Confirmer la réintégration") : t("Confirmer le retour à la cartouche précédente")}
          </AlertTitle>
          <AlertDescription className="flex flex-col gap-3">
            <p>
              {confirmation.action === "install"
                ? t("La version {version} remplacera la cartouche {barcode}. Sa version actuelle sera conservée pour un retour arrière.", { version: restore?.generation ?? "", barcode: barcode ?? "" })
                : operations.find(operation => operation.id === confirmation.id)?.new_media
                  ? t("La cartouche ajoutée sera retirée de la bibliothèque et ses fichiers seront conservés. L’emplacement sera libéré.")
                  : t("La cartouche conservée avant cette réintégration sera rétablie. La version remplacée sera également conservée.")}{" "}{t("Une fenêtre de maintenance sera ouverte puis refermée après vérification du changeur PBS.")}</p>
            <div className="flex flex-wrap gap-2">
              <Button disabled={locked} onClick={() => void submit()}>{t("Confirmer et lancer")}</Button>
              <Button
                variant="outline"
                disabled={pending}
                onClick={() => setConfirmation(null)}
              >{t("Annuler")}</Button>
            </div>
          </AlertDescription>
        </Alert>
      )}
      {!canManage && (
        <p className="text-sm text-muted-foreground">{t("Un administrateur peut lancer ces opérations.")}</p>
      )}
      {error && (
        <Alert variant="destructive">
          <AlertTitle>{t("Opération non confirmée")}</AlertTitle>
          <AlertDescription>{t(error)}</AlertDescription>
        </Alert>
      )}
    </section>
  );
}
