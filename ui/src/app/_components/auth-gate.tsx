"use client";
import {translate as t,useI18n,getLocale} from "@/lib/i18n";

import { useEffect, useState, type FormEvent } from "react";
import { LoginBackdrop } from "./login-backdrop";
import { LoginCartridge } from "./login-cartridge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
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
  // The front panel of the appliance, standing where the streams of the
  // backdrop converge: its display asks who you are, the right half takes the
  // credentials.
  return (
    <main className="lto-login-page">
      <LoginBackdrop />
      <div className="absolute top-4 right-4"><LanguageSelector/></div>
      <section className="lto-chassis lto-login" aria-labelledby="login-title">
        <div className="lto-rack-ear lto-rack-ear-left" aria-hidden="true"><i/><i/></div>
        <div className="lto-rack-ear lto-rack-ear-right" aria-hidden="true"><i/><i/></div>
        <div className="lto-login-display">
          {/* The brand is the heading: it says where you sign in, read aloud as the full sentence. */}
          <h1 id="login-title" className="lto-login-brand">
            {/* The panel is dark in both themes: always the mark drawn for dark backgrounds. */}
            <img src="/brand/vaultcenter-icon-dark.svg" width={961} height={789} alt="" className="lto-login-mark" />
            <span className="vaultcenter-wordmark" aria-hidden="true">Vault<span>Center</span></span>
            <span className="sr-only">{t("Connexion à VaultCenter")}</span>
          </h1>
          <LoginCartridge />
        </div>
        <div className="lto-login-panel">
          <p className="lto-login-intro">{t("Accédez à votre bibliothèque et à vos copies S3.")}</p>
          {error && (
            <p role="alert" className="lto-login-error">
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
            <div className="lto-login-sso">
              <p>{t("Compte d’entreprise")}</p>
              <Button asChild variant="outline" className="w-full">
                <a href="/auth/oidc/login">
                  {t('Continuer avec {provider}',{provider:config.oidc_label || 'SSO'})}
                </a>
              </Button>
            </div>
          )}
        </div>
      </section>
    </main>
  );
}
