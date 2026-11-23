"use client"

import * as React from "react"
import { Select as SelectPrimitive } from "radix-ui"
import { Check, ChevronDown, ChevronUp } from "lucide-react"
import { cn } from "cn"

// Encode every value so an empty filter value remains a selectable item.
const encode = (value: string) => `option:${value}`

type SelectProps = Omit<React.ComponentPropsWithoutRef<"button">, "value" | "defaultValue" | "onChange" | "size"> & {
  value?: string
  defaultValue?: string
  onValueChange?: (value: string) => void
  size?: "sm" | "default"
  icon?: React.ReactNode
}

function Select({ children, className, value, defaultValue = "", onValueChange, disabled, name, size = "default", icon, ...props }: SelectProps) {
  const [localValue, setLocalValue] = React.useState(defaultValue)
  const selected = value ?? localValue
  const trigger = React.useRef<HTMLButtonElement>(null)
  const [open, setOpen] = React.useState(false)
  React.useEffect(() => {
    const form = trigger.current?.form
    const reset = () => setLocalValue(defaultValue)
    form?.addEventListener("reset", reset)
    return () => form?.removeEventListener("reset", reset)
  }, [defaultValue])
  return (
    <div data-slot="select-wrapper" className={cn("min-w-0 w-fit max-w-full", className)}>
      <SelectPrimitive.Root value={encode(selected)} disabled={disabled} open={open}
        onOpenChange={next => { if (!next || !trigger.current?.matches(":disabled")) setOpen(next) }}
        onValueChange={next => {
          if (trigger.current?.matches(":disabled")) return
          const raw = next.slice("option:".length)
          setLocalValue(raw)
          onValueChange?.(raw)
        }}>
        <SelectPrimitive.Trigger {...props} ref={trigger} data-slot="select-trigger" data-size={size} data-value={selected}
          className="flex h-8 w-full min-w-0 items-center justify-between gap-2 rounded-lg border border-input bg-transparent py-1 pr-2.5 pl-2.5 text-left text-sm text-foreground outline-none transition-colors focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50 aria-invalid:border-destructive data-[size=sm]:h-7 dark:bg-input/30 hover:bg-accent dark:hover:bg-input/50 [&>span]:truncate">
          {icon}
          <SelectPrimitive.Value />
          <SelectPrimitive.Icon asChild><ChevronDown className="ml-auto size-4 shrink-0 text-muted-foreground" aria-hidden="true" /></SelectPrimitive.Icon>
        </SelectPrimitive.Trigger>
        <SelectPrimitive.Portal>
          <SelectPrimitive.Content onEscapeKeyDown={event => { event.preventDefault(); event.stopPropagation(); setOpen(false) }} position="popper" sideOffset={5} collisionPadding={8} data-slot="select-content"
            className="z-[70] max-h-[min(20rem,var(--radix-select-content-available-height))] min-w-[var(--radix-select-trigger-width)] max-w-[calc(100vw-1rem)] overflow-hidden rounded-lg border border-border bg-popover text-popover-foreground shadow-lg">
            <SelectPrimitive.ScrollUpButton className="flex h-6 items-center justify-center bg-popover"><ChevronUp className="size-4" /></SelectPrimitive.ScrollUpButton>
            <SelectPrimitive.Viewport className="p-1">{children}</SelectPrimitive.Viewport>
            <SelectPrimitive.ScrollDownButton className="flex h-6 items-center justify-center bg-popover"><ChevronDown className="size-4" /></SelectPrimitive.ScrollDownButton>
          </SelectPrimitive.Content>
        </SelectPrimitive.Portal>
      </SelectPrimitive.Root>
      {name && <input type="hidden" name={name} value={selected} disabled={disabled} />}
    </div>
  )
}

function SelectOption({ value, children, className, ...props }: React.ComponentPropsWithoutRef<typeof SelectPrimitive.Item>) {
  return (
    <SelectPrimitive.Item {...props} value={encode(value)} data-value={value} data-slot="select-option"
      className={cn("relative flex min-h-8 cursor-pointer select-none items-center rounded-md py-1.5 pr-8 pl-2 text-sm break-words outline-none data-[highlighted]:bg-accent data-[highlighted]:text-accent-foreground data-[state=checked]:bg-accent data-[state=checked]:text-accent-foreground data-[state=checked]:font-medium data-[disabled]:pointer-events-none data-[disabled]:opacity-50", className)}>
      <SelectPrimitive.ItemText>{children}</SelectPrimitive.ItemText>
      <SelectPrimitive.ItemIndicator className="absolute right-2 flex items-center"><Check className="size-4" aria-hidden="true" /></SelectPrimitive.ItemIndicator>
    </SelectPrimitive.Item>
  )
}

export { Select, SelectOption }
