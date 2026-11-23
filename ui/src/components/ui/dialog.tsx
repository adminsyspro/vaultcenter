"use client"
import {useI18n} from "@/lib/i18n"

import * as React from "react"
import { cn } from "cn"
import { Dialog as DialogPrimitive } from "radix-ui"

import { Button } from "@/components/ui/button"
import { XIcon, type LucideIcon } from "lucide-react"

function Dialog({
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Root>) {
  return <DialogPrimitive.Root data-slot="dialog" {...props} />
}

function DialogTrigger({
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Trigger>) {
  return <DialogPrimitive.Trigger data-slot="dialog-trigger" {...props} />
}

function DialogPortal({
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Portal>) {
  return <DialogPrimitive.Portal data-slot="dialog-portal" {...props} />
}

function DialogClose({
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Close>) {
  return <DialogPrimitive.Close data-slot="dialog-close" {...props} />
}

function DialogOverlay({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Overlay>) {
  return (
    <DialogPrimitive.Overlay
      data-slot="dialog-overlay"
      className={cn(
        "fixed inset-0 isolate z-50 bg-black/10 duration-100 supports-backdrop-filter:backdrop-blur-xs data-open:animate-in data-open:fade-in-0 data-closed:animate-out data-closed:fade-out-0",
        className
      )}
      {...props}
    />
  )
}

function DialogContent({
  className,
  children,
  showCloseButton = true,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Content> & {
  showCloseButton?: boolean
}) {
  const {t}=useI18n();
  const parts = React.Children.toArray(children)
  const isHeader = (child: React.ReactNode) => React.isValidElement(child) && child.type === DialogHeader
  const isClose = (child: React.ReactNode) => React.isValidElement(child) && child.type === DialogClose
  const descriptions: React.ReactNode[] = []
  const header = parts.filter(isHeader).map(child => {
    const element = child as React.ReactElement<React.ComponentProps<typeof DialogHeader>>
    const content = React.Children.toArray(element.props.children).filter(part => {
      if (React.isValidElement(part) && part.type === DialogDescription) {
        descriptions.push(part)
        return false
      }
      return true
    })
    return React.cloneElement(element, {}, content)
  })
  const close = parts.filter(isClose)
  const body = parts.filter(child => !isHeader(child) && !isClose(child))
  return (
    <DialogPortal>
      <DialogOverlay />
      <DialogPrimitive.Content
        data-slot="dialog-content"
        className={cn(
          "lto-dialog fixed top-1/2 left-1/2 z-50 flex max-h-[90dvh] flex-col w-full max-w-[calc(100%-2rem)] -translate-x-1/2 -translate-y-1/2 gap-4 rounded-xl bg-popover p-4 text-sm text-popover-foreground duration-100 outline-none sm:max-w-sm data-open:animate-in data-open:fade-in-0 data-open:zoom-in-95 data-closed:animate-out data-closed:fade-out-0 data-closed:zoom-out-95",
          className,
          "gap-0 overflow-hidden p-0"
        )}
        {...props}
      >
        {/* The heading is the appliance's LCD, like the activity log: title, then its description. */}
        <div data-slot="dialog-heading" className="lto-dialog-lcd relative shrink-0">
          {header}
          {descriptions.length > 0 && <div data-slot="dialog-lcd-detail" className="lto-dialog-lcd-detail">{descriptions}</div>}
          {close}
        {showCloseButton && (
          <DialogPrimitive.Close data-slot="dialog-close" asChild>
            <Button
              variant="ghost"
              className="lto-dialog-close absolute top-3 right-3"
              size="icon-sm"
            >
              <XIcon
              />
              <span className="sr-only">{t('Fermer')}</span>
            </Button>
          </DialogPrimitive.Close>
        )}
        </div>
        <div data-slot="dialog-body" className="flex min-h-0 min-w-0 flex-col gap-4 overflow-y-auto overscroll-contain p-4">
          {body}
        </div>
      </DialogPrimitive.Content>
    </DialogPortal>
  )
}

/** `icon` is still accepted from callers but no longer drawn: the LCD prompt replaces it.
 *  The prompt and cursor are decorative elements, kept out of the dialog's accessible name. */
function DialogHeader({ className, children, icon: _icon, ...props }: React.ComponentProps<"div"> & { icon?: LucideIcon }) {
  return (
    <div
      data-slot="dialog-header"
      className={cn("flex min-h-14 min-w-0 shrink-0 items-center gap-3 px-4 pr-12 [&_[data-slot=dialog-title]]:min-w-0 [&_[data-slot=dialog-title]]:truncate [&_[data-slot=dialog-title]]:leading-normal", className)}
      {...props}
    >
      <span aria-hidden="true" className="lto-dialog-prompt">&gt;</span>
      {children}
      <span aria-hidden="true" className="lto-dialog-cursor" />
    </div>
  )
}

function DialogFooter({
  className,
  showCloseButton = false,
  children,
  ...props
}: React.ComponentProps<"div"> & {
  showCloseButton?: boolean
}) {
  const {t}=useI18n();
  return (
    <div
      data-slot="dialog-footer"
      className={cn(
        "-mx-4 -mb-4 flex flex-col-reverse gap-2 rounded-b-xl border-t bg-muted/50 p-4 sm:flex-row sm:justify-end",
        className
      )}
      {...props}
    >
      {children}
      {showCloseButton && (
        <DialogPrimitive.Close asChild>
          <Button variant="outline">{t('Fermer')}</Button>
        </DialogPrimitive.Close>
      )}
    </div>
  )
}

function DialogTitle({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Title>) {
  return (
    <DialogPrimitive.Title
      data-slot="dialog-title"
      className={cn(
        "font-heading text-base leading-none font-medium",
        className
      )}
      {...props}
    />
  )
}

function DialogDescription({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Description>) {
  return (
    <DialogPrimitive.Description
      data-slot="dialog-description"
      className={cn(
        "text-sm text-muted-foreground *:[a]:underline *:[a]:underline-offset-3 *:[a]:hover:text-foreground",
        className
      )}
      {...props}
    />
  )
}

export {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogOverlay,
  DialogPortal,
  DialogTitle,
  DialogTrigger,
}
