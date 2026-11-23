"use client";
import { translate as t, useI18n, getLocale } from "@/lib/i18n";

import { Avatar, DropdownMenu } from "radix-ui";
import { LogOut, UserRound } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useRef, useState } from "react";
import { request, type AuthUser, type ServiceStatus } from "@/lib/api";
import { versionLabel } from "@/lib/version";

const itemClassName = "flex cursor-pointer items-center gap-2 rounded-md px-3 py-2 text-sm outline-none data-highlighted:bg-accent data-highlighted:text-accent-foreground data-disabled:pointer-events-none data-disabled:opacity-50";

export function AccountMenu({ user, onLogout, logoutBusy }: {
  user: AuthUser;
  onLogout: () => Promise<void>;
  logoutBusy: boolean;
}) {
  useI18n();
  const initials = Array.from(user.username.trim()).slice(0, 2).join("").toLocaleUpperCase(getLocale());
  const [version, setVersion] = useState<string | null>(null);
  const loading = useRef(false);
  const onOpenChange = (open: boolean) => {
    if (!open || version || loading.current) return;
    loading.current = true;
    request<ServiceStatus>("/v1/status").then(s => setVersion(versionLabel(s)), () => {}).finally(() => { loading.current = false; });
  };
  return (
    <DropdownMenu.Root onOpenChange={onOpenChange}>
      <DropdownMenu.Trigger asChild>
        <Button
          variant="ghost"
          size="icon-lg"
          className="rounded-full"
          aria-label={t("Menu du compte")}
          title={user.username}
          disabled={logoutBusy}
        >
          <Avatar.Root aria-hidden="true" className="flex size-9 overflow-hidden rounded-full">
            <Avatar.Fallback className="flex size-full items-center justify-center bg-primary/15 text-xs font-semibold text-foreground">
              {initials || <UserRound className="size-4" />}
            </Avatar.Fallback>
          </Avatar.Root>
        </Button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content align="end" sideOffset={8} className="z-50 w-60 max-w-[calc(100vw-2rem)] rounded-xl bg-popover p-1 text-popover-foreground shadow-md">
          <DropdownMenu.Label className="space-y-1 px-3 py-2">
            <p className="break-words text-sm font-medium">{user.username}</p>
            <p className="text-xs font-normal text-muted-foreground">{user.role === "admin" ? t("Administrateur") : t("Lecture seule")}</p>
          </DropdownMenu.Label>
          <DropdownMenu.Separator className="my-1 h-px bg-border" />
          <DropdownMenu.Item asChild className={itemClassName}>
            <a href="#profile"><UserRound aria-hidden="true" className="size-4" />{t("Profil")}</a>
          </DropdownMenu.Item>
          <DropdownMenu.Item disabled={logoutBusy} onSelect={() => void onLogout()} className={itemClassName}>
            <LogOut aria-hidden="true" className="size-4" />{t("Se déconnecter")}</DropdownMenu.Item>
          {version && <>
            <DropdownMenu.Separator className="my-1 h-px bg-border" />
            <p className="px-3 py-1.5 text-xs text-muted-foreground tabular-nums">{version}</p>
          </>}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
