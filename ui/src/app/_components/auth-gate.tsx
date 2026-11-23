"use client";
import {translate as t,useI18n,getLocale} from "@/lib/i18n";

import { useEffect, useState, type FormEvent } from "react";
import { VaultCenterLogo } from "./vaultcenter-mark";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Card,
  CardHeader,
  CardTitle,
  CardDescription,
  CardContent,
} from "@/components/ui/card";
import { request, setCsrfToken, type AuthUser, type Session } from "@/lib/api";
import {LanguageSelector} from "./language-selector";
import { Console } from "./console";

export function AuthGate() {
  const {t}=useI18n();
  const [user, setUser] = useState<AuthUser | null>(null);
  const [loading, setLoading] = useState(true);
  const [config, setConfig] = useState<{
    oidc: boolean;
    oidc_label: string;
  } | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  function signedOut() {
    setCsrfToken(null);
    setUser(null);
  }
  function accept(session: Session) {
    setCsrfToken(session.csrf_token);
    setUser(session.user);
    setError("");
  }
  useEffect(() => {
    let active = true;
    window.addEventListener("cold-vtl-unauthorized", signedOut);
    Promise.all([
      request<{ oidc: boolean; oidc_label: string }>("/auth/config"),
      request<Session>("/auth/me").catch(() => null),
    ])
      .then(([config, session]) => {
        if (active) {
          setConfig(config);
          if (session) accept(session);
        }
      })
      .catch(() => {
        if (active)
          setError(
            t("Le service est inaccessible. Rechargez la page pour réessayer."),
          );
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    if (new URLSearchParams(location.search).has("login_error")) {
      setError(
        t("Connexion SSO refusée ou expirée. Vérifiez votre accès auprès de l’administrateur, puis réessayez."),
      );
      history.replaceState(null, "", location.pathname + location.hash);
    }
    return () => {
      active = false;
      window.removeEventListener("cold-vtl-unauthorized", signedOut);
    };
  }, []);
  async function login(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = event.currentTarget;
    const fields = new FormData(form);
    setBusy(true);
    setError("");
    try {
      accept(
        await request<Session>("/auth/login", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            username: fields.get("username"),
            password: fields.get("password"),
          }),
        }),
      );
      form.reset();
    } catch (cause) {
      setError(
        cause instanceof Error ? cause.message : t("Connexion impossible."),
      );
    } finally {
      setBusy(false);
    }
  }
  async function logout() {
    setBusy(true);
    try {
      await request("/auth/logout", { method: "POST" });
      signedOut();
    } catch {
      setError(t("La déconnexion a échoué. Réessayez."));
    } finally {
      setBusy(false);
    }
  }
  if (loading)
    return (
      <main className="grid min-h-svh place-items-center" role="status">
        {t("Chargement de la connexion…")}</main>
    );
  if (user)
    return (
      <>
        <Console
          user={user}
          onLogout={logout}
          logoutBusy={busy}
          onSessionEnded={signedOut}
        />
        {error && (
          <p
            role="alert"
            className="fixed bottom-4 right-4 rounded border bg-background p-4"
          >
            {t(error)}
          </p>
        )}
      </>
    );
  return (
    <main className="relative grid min-h-svh place-items-center bg-muted/30 p-4 pt-16 sm:pt-4">
      <div className="absolute top-4 right-4"><LanguageSelector/></div>
      <Card className="w-full max-w-md">
        <CardHeader>
          <VaultCenterLogo className="mb-4 w-full rounded-md" />
          <CardTitle className="text-2xl">
            <h1>{t("Connexion à VaultCenter")}</h1>
          </CardTitle>
          <CardDescription>
            {t("Accédez à votre bibliothèque et à vos copies S3.")}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {t(error)}
            </p>
          )}
          <form onSubmit={login} className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="login-user">{t("Utilisateur local")}</Label>
              <Input
                id="login-user"
                name="username"
                autoComplete="username"
                required
                maxLength={64}
                disabled={busy}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="login-password">{t("Mot de passe")}</Label>
              <Input
                id="login-password"
                name="password"
                type="password"
                autoComplete="current-password"
                required
                maxLength={1024}
                disabled={busy}
              />
            </div>
            <Button type="submit" className="w-full" disabled={busy || !config}>
              {busy ? t("Connexion…") : t("Se connecter")}
            </Button>
          </form>
          {config?.oidc && (
            <div className="space-y-3 border-t pt-5">
              <p className="text-center text-sm text-muted-foreground">
                {t("Compte d’entreprise")}</p>
              <Button asChild variant="outline" className="w-full">
                <a href="/auth/oidc/login">
                  {t('Continuer avec {provider}',{provider:config.oidc_label || 'SSO'})}
                </a>
              </Button>
            </div>
          )}
        </CardContent>
      </Card>
    </main>
  );
}
