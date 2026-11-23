/** Shared brand symbol. The adjacent VaultCenter label supplies its accessible name. */
export function VaultCenterMark({ className = "size-9" }: { className?: string }) {
  return <span aria-hidden="true" className={`vaultcenter-mark shrink-0 ${className}`}>
    <img src="/brand/vaultcenter-icon-light.svg" width={961} height={789} alt="" className="vaultcenter-mark-light size-full object-contain" />
    <img src="/brand/vaultcenter-icon-dark.svg" width={961} height={789} alt="" className="vaultcenter-mark-dark size-full object-contain" />
  </span>;
}

export function VaultCenterWordmark({ className = "" }: { className?: string }) {
  return <span className={`vaultcenter-wordmark ${className}`}>Vault<span>Center</span></span>;
}

/** The owner's approved horizontal artwork, preserved without reconstruction. */
export function VaultCenterLogo({ className = "" }: { className?: string }) {
  return <img src="/brand/vaultcenter-horizontal-approved.png" width={468} height={150} alt="VaultCenter" className={`h-auto max-w-full object-contain ${className}`} />;
}
