"use client";
import {useI18n} from '@/lib/i18n';
import {LtoBarcodeLabel} from './lto-barcode-label';
import {LtoReel} from './lto-reel';

/**
 * The sign-in page's emblem: the very cartridge the console draws for a tape
 * being written, reel turning and fill growing, as a backup runs.
 * Decorative only; it stands still when reduced motion is requested.
 */
export function LoginCartridge() {
  const {t}=useI18n();
  return (
    <div className="lto-cartridge lto-login-cartridge" data-running="true" aria-hidden="true">
      <div className="lto-cartridge-top"><span>LTO-8</span><span className="lto-ribs" /></div>
      <LtoReel/>
      <span className="lto-label">
        <LtoBarcodeLabel barcode="VLT001L8"/>
        <span className="lto-label-status">{t("Écriture en cours")}</span>
      </span>
      <div className="lto-usage"><div className="lto-gauge"><span className="lto-login-fill"/></div></div>
    </div>
  );
}
