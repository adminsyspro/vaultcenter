import { type Snapshot } from "@/lib/api";
import { Jobs } from "./jobs";
import { DashboardCharts } from "./dashboard-charts";

export function Overview({ data, onJobs }: {
  data: Snapshot; onJobs: () => void;
}) {
  const { jobs } = data;
  return <div className="space-y-6">
    <DashboardCharts data={data} />
    <Jobs jobs={jobs} compact onShowAll={onJobs} />
  </div>;
}
