"use client";
import {translate as t,useI18n,getLocale} from "@/lib/i18n";

import { CircleHelp } from "lucide-react";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

export function VersionTooltip({ version, restore = false }: { version?: number; restore?: boolean }) {
  const {t}=useI18n();
  return <Tooltip>
    <TooltipTrigger asChild>
      <button type="button" aria-label={version === undefined ? t("À propos des versions") : t('Version {version} : explication',{version})} className="inline-flex items-center gap-1.5 rounded-sm text-xs text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring">
        {version !== undefined && <span>{t('Version {version}',{version})}</span>}
        <CircleHelp aria-hidden="true" className="size-3.5 shrink-0" />
      </button>
    </TooltipTrigger>
    <TooltipContent sideOffset={6} className="block max-w-72 space-y-2 leading-relaxed">
      <p>{t("Une version correspond à un état du contenu de la cartouche. Si son contenu change, une nouvelle version permet de le distinguer de la précédente.")}</p>
      <p>{restore ? t("Choisissez la version sauvegardée que vous souhaitez récupérer. La date du dernier export, lorsqu’elle est connue, aide à retrouver la bonne copie.") : t("Version 1, Version 2… désignent la même cartouche à des états différents. Les versions sauvegardées sur S3 sont disponibles dans Détails → Restaurer.")}</p>
    </TooltipContent>
  </Tooltip>;
}
