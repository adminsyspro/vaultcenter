import { Button } from "@/components/ui/button";

export function SectionNav<T extends string>({ label, value, onChange, items }: {
  label: string; value: T; onChange: (value: T) => void;
  items: { value: T; label: string }[];
}) {
  return <div role="group" aria-label={label} className="vc-section-nav flex flex-wrap gap-1 rounded-lg border bg-muted/40 p-1">
    {items.map(item => <Button key={item.value} size="sm" variant={value === item.value ? "secondary" : "ghost"}
      aria-pressed={value === item.value} onClick={() => onChange(item.value)}
      className={value === item.value ? "bg-background shadow-sm" : "text-muted-foreground"}>
      {item.label}
    </Button>)}
  </div>;
}
