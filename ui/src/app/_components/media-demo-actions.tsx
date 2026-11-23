"use client";
import { localizedToast as toast } from "./providers";
import { Checkbox } from "@/components/ui/checkbox";
import { Select, SelectOption } from "@/components/ui/select";
import { useI18n } from "@/lib/i18n";

/** Local demonstration only: which tape the drive writes to, and whether the next S3 send fails. */
export function MediaDemoActions({ barcodes, drive, failNext }: { barcodes: string[]; drive: string; failNext: boolean }) {
  const { t } = useI18n();
  async function demo(path: string) {
    try { const response = await fetch(path, { method: "POST" }); if (!response.ok) throw Error(); }
    catch { toast.error(t("Action de démonstration indisponible")); }
  }
  return <>
    <Select aria-label={t("Cartouche dans le lecteur")} value={drive}
      onValueChange={barcode => void demo(`/__demo/drive?barcode=${encodeURIComponent(barcode)}`)}>
      {barcodes.map(barcode => <SelectOption key={barcode} value={barcode}>{t("Lecteur : {barcode}", { barcode })}</SelectOption>)}
    </Select>
    <label className="lto-demo-toggle">
      <Checkbox checked={failNext} onCheckedChange={value => void demo(`/__demo/fail-next?enabled=${value === true ? 1 : 0}`)} />
      {t("Faire échouer le prochain envoi S3")}
    </label>
  </>;
}
