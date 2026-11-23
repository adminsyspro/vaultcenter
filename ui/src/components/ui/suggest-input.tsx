"use client"

import * as React from "react"
import { Popover } from "radix-ui"
import { Check } from "lucide-react"
import { Input } from "@/components/ui/input"

type SuggestInputProps = Omit<React.ComponentProps<typeof Input>, "value" | "onChange"> & {
  value: string
  onValueChange: (value: string) => void
  suggestions: string[]
}

// Editable combobox: arbitrary bucket names are valid, including undiscovered ones.
export function SuggestInput({ value, onValueChange, suggestions, disabled, ...props }: SuggestInputProps) {
  const [open, setOpen] = React.useState(false)
  const [active, setActive] = React.useState(-1)
  const input = React.useRef<HTMLInputElement>(null)
  const listId = React.useId()
  const matches = suggestions.filter(item => item.toLocaleLowerCase().includes(value.toLocaleLowerCase()))
  const visible = open && !disabled && matches.length > 0
  const choose = (next: string) => { onValueChange(next); setOpen(false); setActive(-1); input.current?.focus() }
  React.useEffect(() => {
    if (visible && active >= 0) document.getElementById(`${listId}-${active}`)?.scrollIntoView({block:"nearest"})
  }, [active, visible, listId])
  return <Popover.Root open={visible} onOpenChange={setOpen}>
    <Popover.Anchor asChild><Input {...props} ref={input} role="combobox" aria-autocomplete="list" aria-expanded={visible} aria-controls={visible ? listId : undefined} aria-activedescendant={visible && active >= 0 ? `${listId}-${active}` : undefined}
      value={value} disabled={disabled} onFocus={() => setOpen(true)} onClick={() => setOpen(true)}
      onChange={event => { onValueChange(event.target.value); setOpen(true); setActive(-1) }}
      onKeyDown={event => {
        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault(); setOpen(true)
          setActive(index => matches.length ? (index + (event.key === "ArrowDown" ? 1 : -1) + matches.length) % matches.length : -1)
        } else if (event.key === "Enter" && visible && active >= 0 && matches[active]) {
          event.preventDefault(); choose(matches[active])
        } else if (event.key === "Escape" && visible) {
          event.preventDefault(); event.stopPropagation(); setOpen(false)
        } else if (event.key === "Tab") setOpen(false)
      }}/></Popover.Anchor>
    <Popover.Portal><Popover.Content id={listId} role="listbox" aria-label={props["aria-label"] ?? value} sideOffset={5} collisionPadding={8}
      onEscapeKeyDown={event => { event.preventDefault(); event.stopPropagation(); setOpen(false) }}
      onOpenAutoFocus={event => event.preventDefault()} onCloseAutoFocus={event => event.preventDefault()}
      onInteractOutside={event => { if (event.target === input.current) event.preventDefault() }}
      className="z-[70] max-h-[min(16rem,var(--radix-popover-content-available-height))] w-[var(--radix-popover-trigger-width)] max-w-[calc(100vw-1rem)] overflow-y-auto rounded-lg border border-border bg-popover p-1 text-popover-foreground shadow-lg">
      {matches.map((item, index) => <div key={item} id={`${listId}-${index}`} role="option" aria-selected={value === item} data-highlighted={index === active ? "" : undefined}
        className="flex min-h-8 cursor-pointer items-center justify-between gap-2 rounded-md px-2 py-1.5 text-sm break-all outline-none hover:bg-accent hover:text-accent-foreground data-[highlighted]:bg-accent data-[highlighted]:text-accent-foreground"
        onPointerMove={() => setActive(index)} onPointerDown={event => event.preventDefault()} onClick={() => choose(item)}>
        {item}{value === item && <Check className="size-4 shrink-0 text-primary" aria-hidden="true"/>}
      </div>)}
    </Popover.Content></Popover.Portal>
  </Popover.Root>
}
