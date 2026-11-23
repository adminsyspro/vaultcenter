"use client";

import {useEffect, type ReactNode} from "react";
import {initializeLanguage, useI18n} from "@/lib/i18n";
import { ThemeProvider, useTheme } from "next-themes";
import { Toaster, toast, type Action, type ExternalToast } from "sonner";
import { TooltipProvider } from "@/components/ui/tooltip";

type ToastContent = ReactNode | (() => ReactNode);

function LocalizedToastText({value}: {value: ToastContent}) {
  const {t} = useI18n();
  const content = typeof value === "function" ? value() : value;
  return typeof content === "string" ? t(content) : content;
}

function localizeContent(value: ToastContent): ToastContent {
  return typeof value === "string" || typeof value === "function"
    ? <LocalizedToastText value={value}/>
    : value;
}

function localizeAction(value: ExternalToast["action"]): ExternalToast["action"] {
  if (value && typeof value === "object" && "label" in value && "onClick" in value) {
    const action = value as Action;
    return {...action, label: localizeContent(action.label) as ReactNode};
  }
  return value;
}

function localizeOptions(options?: ExternalToast): ExternalToast | undefined {
  if (!options) return options;
  return {
    ...options,
    description: localizeContent(options.description),
    action: localizeAction(options.action),
    cancel: localizeAction(options.cancel),
  };
}

// Subscribe inside the content nodes, not by republishing toast objects: Sonner
// restarts its expiry effect when a toast object changes. IDs, timers and action
// callbacks therefore remain owned by Sonner throughout a language switch.
const wrapToast = (notify: typeof toast.success): typeof toast.success =>
  (message, options) => notify(localizeContent(message), localizeOptions(options));
export const localizedToast: typeof toast = Object.assign(
  (message: Parameters<typeof toast>[0], options?: ExternalToast) => toast(localizeContent(message), localizeOptions(options)),
  toast,
  {
    success: wrapToast(toast.success),
    error: wrapToast(toast.error),
    info: wrapToast(toast.info),
    warning: wrapToast(toast.warning),
    message: wrapToast(toast.message),
    loading: wrapToast(toast.loading),
  },
);

function Notifications() {
  const { resolvedTheme } = useTheme();
  const {t} = useI18n();
  return (
    <Toaster
      theme={resolvedTheme === "dark" ? "dark" : "light"}
      closeButton
      containerAriaLabel={t("Notifications")}
      toastOptions={{closeButtonAriaLabel: t("Fermer la notification")}}
      position="bottom-right"
    />
  );
}

export function Providers({ children }: { children: React.ReactNode }) {
  useEffect(initializeLanguage,[]);
  return (
    <ThemeProvider
      attribute="class"
      defaultTheme="dark"
      enableSystem
      disableTransitionOnChange
    >
      <TooltipProvider>
        {children}
        <Notifications />
      </TooltipProvider>
    </ThemeProvider>
  );
}
