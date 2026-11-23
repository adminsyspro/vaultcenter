// Maintenance status already polled by the power control and the maintenance
// dialog, shared with the LCD journal without another (PBS-guarded) request.
export type MaintenanceSnapshot = { phase: string; ready: boolean; message?: string | null };
const name = "cold-vtl-maintenance-status";

export function publishMaintenance(vtlId: string | null, status: MaintenanceSnapshot) {
  if (vtlId) window.dispatchEvent(new CustomEvent(name, { detail: { vtlId, status } }));
}

export function onMaintenance(vtlId: string | null, listener: (status: MaintenanceSnapshot) => void) {
  const handle = (event: Event) => {
    const detail = (event as CustomEvent<{ vtlId: string; status: MaintenanceSnapshot }>).detail;
    if (detail?.vtlId === vtlId) listener(detail.status);
  };
  window.addEventListener(name, handle);
  return () => window.removeEventListener(name, handle);
}

/** LCD line for a maintenance phase; null when there is nothing to report. */
export function maintenanceLine(status: MaintenanceSnapshot): string | null {
  switch (status.phase) {
    case "entering": return "Maintenance · arrêt des services de la bibliothèque…";
    case "maintenance": return status.ready ? "Maintenance active · services arrêtés, cartouches modifiables" : "Maintenance · verrou non confirmé";
    case "leaving": return "Maintenance · redémarrage des services…";
    case "failed": return "Maintenance interrompue · reprise requise";
    case "online": return "Bibliothèque en service";
    default: return null;
  }
}
