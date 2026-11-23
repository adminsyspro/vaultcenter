"use client";
import { translate as t, useI18n, getLocale } from "@/lib/i18n";
import { useEffect, useState, type FormEvent } from "react";
import { UserRound } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Card,
  CardHeader,
  CardTitle,
  CardContent,
  CardDescription,
} from "@/components/ui/card";
import {
  Select,
  SelectOption,
} from "@/components/ui/select";
import { request, type AuthUser } from "@/lib/api";

type Token = { id: string; name: string; expires_at: number };
export function Account({
  user,
  onSessionEnded,
  view = "access",
}: {
  user: AuthUser;
  onSessionEnded: () => void;
  view?: "profile" | "access";
}) {
  useI18n();
  const [tokens, setTokens] = useState<Token[]>([]);
  const [users, setUsers] = useState<AuthUser[]>([]);
  const [secret, setSecret] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  async function refresh() {
    if (view === "profile") {
      setTokens(await request<Token[]>("/auth/tokens"));
    } else if (user.role === "admin") {
      setUsers(await request<AuthUser[]>("/auth/users"));
    }
  }
  useEffect(() => {
    void refresh().catch(() => setError(t("Impossible de charger les accès.")));
  }, [user.id, user.role, view]); // eslint-disable-line react-hooks/exhaustive-deps
  async function action(work: () => Promise<void>) {
    setBusy(true);
    setError("");
    try {
      await work();
      await refresh();
    } catch (cause) {
      setError(
        cause instanceof Error ? cause.message : t("Opération impossible."),
      );
    } finally {
      setBusy(false);
    }
  }
  function submit(
    event: FormEvent<HTMLFormElement>,
    work: (data: FormData) => Promise<void>,
  ) {
    event.preventDefault();
    const form = event.currentTarget;
    const fields = new FormData(form);
    void action(async () => {
      await work(fields);
      form.reset();
    });
  }
  const post = <T,>(path: string, data: unknown) =>
    request<T>(path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(data),
    });
  return (
    <div className="space-y-6">
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {t(error)}
        </p>
      )}
      {view === "profile" && <Card role="region" aria-labelledby="account-profile-title">
        <CardHeader>
          <CardTitle><h2 id="account-profile-title">{t("Profil")}</h2></CardTitle>
          <CardDescription>{t("Votre identité et la sécurité de votre compte.")}</CardDescription>
        </CardHeader>
        <CardContent className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(0,2fr)]">
          <div className="space-y-5">
            <div className="flex items-center gap-3">
              <div className="flex size-10 shrink-0 items-center justify-center rounded-full bg-muted">
                <UserRound aria-hidden="true" className="size-5 text-muted-foreground" />
              </div>
              <div className="min-w-0">
                <p className="break-words text-lg font-semibold">{user.username}</p>
                <p className="text-sm text-muted-foreground">{user.issuer ? t("Compte OIDC") : t("Compte local")}</p>
              </div>
            </div>
            <dl className="flex flex-wrap items-center gap-3 text-sm">
              <dt className="text-muted-foreground">{t("Rôle")}</dt>
              <dd><Badge variant="secondary">{user.role === "admin" ? t("Administrateur") : t("Lecture seule")}</Badge></dd>
            </dl>
          </div>
          <div className="space-y-4 border-t pt-6 lg:border-t-0 lg:border-l lg:pt-0 lg:pl-6">
            {user.issuer ? (
              <div className="space-y-2 text-sm">
                <h3 className="font-medium">{t("Connexion SSO")}</h3>
                <p className="text-muted-foreground">{t("Votre mot de passe est géré par votre fournisseur d’identité.")}</p>
                <p className="text-muted-foreground">{t("La déconnexion ferme la session Cold VTL. Pour changer de compte SSO, déconnectez-vous également de Keycloak.")}</p>
              </div>
            ) : (
              <>
                <div className="space-y-1">
                  <h3 className="text-sm font-medium">{t("Changer le mot de passe")}</h3>
                  <p className="text-sm text-muted-foreground">{t("12 caractères minimum. Le changement révoque toutes vos sessions et tous vos jetons.")}</p>
                </div>
                <form
                  className="space-y-4"
                  onSubmit={(e) =>
                    submit(e, async (f) => {
                      await post("/auth/password", {
                        current_password: f.get("current"),
                        new_password: f.get("next"),
                      });
                      onSessionEnded();
                    })
                  }
                >
                  <div className="grid gap-4 sm:grid-cols-2">
                    <div className="space-y-2">
                      <Label htmlFor="current-password">{t("Mot de passe actuel")}</Label>
                      <Input
                        id="current-password"
                        name="current"
                        type="password"
                        autoComplete="current-password"
                        required
                      />
                    </div>
                    <div className="space-y-2">
                      <Label htmlFor="next-password">{t("Nouveau mot de passe")}</Label>
                      <Input
                        id="next-password"
                        name="next"
                        type="password"
                        autoComplete="new-password"
                        minLength={12}
                        maxLength={1024}
                        required
                      />
                    </div>
                  </div>
                  <Button disabled={busy}>{t("Changer le mot de passe")}</Button>
                </form>
              </>
            )}
          </div>
        </CardContent>
      </Card>}
      {view === "profile" && <Card role="region" aria-labelledby="account-tokens-title">
        <CardHeader>
          <CardTitle><h2 id="account-tokens-title">{t("Jetons API et CLI")}</h2></CardTitle>
          <CardDescription>
            {t("Un jeton donne les droits de votre compte. Copiez-le à sa création et conservez-le dans un fichier protégé.")} </CardDescription>
        </CardHeader>
        <CardContent className="space-y-5">
          <h3 className="text-sm font-medium">{t("Nouveau jeton")}</h3>
          <form
            className="flex flex-wrap items-end gap-3"
            onSubmit={(e) =>
              submit(e, async (f) => {
                const result = await post<{ token: string }>("/auth/tokens", {
                  name: f.get("name"),
                  days: Number(f.get("days")),
                });
                setSecret(result.token);
              })
            }
          >
            <div className="space-y-2">
              <Label htmlFor="token-name">{t("Nom du jeton")}</Label>
              <Input
                id="token-name"
                name="name"
                required
                maxLength={80}
                placeholder={t("CLI personnel")}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="token-days">{t("Durée (jours)")}</Label>
              <Input
                id="token-days"
                name="days"
                type="number"
                min={1}
                max={90}
                defaultValue={30}
                required
                className="w-28"
              />
            </div>
            <Button disabled={busy}>{t("Créer un jeton")}</Button>
          </form>
          {secret && (
            <div className="space-y-2 rounded border p-4">
              <Label htmlFor="new-token">{t("Jeton à copier maintenant")}</Label>
              <Input
                id="new-token"
                readOnly
                value={secret}
                className="font-mono"
                onFocus={(e) => e.currentTarget.select()}
              />
              <Button variant="outline" onClick={() => setSecret("")}>
                {t("Masquer le jeton")} </Button>
            </div>
          )}
          <div className="space-y-3 border-t pt-5">
            <h3 className="text-sm font-medium">{t("Vos jetons")}</h3>
            <ul className="divide-y">
              {tokens.map((token) => (
                <li
                  key={token.id}
                  className="flex flex-wrap items-center justify-between gap-3 py-3 first:pt-0 last:pb-0"
                >
                  <div className="min-w-0 text-sm">
                    <p className="break-words font-medium">{token.name}</p>
                    <p className="text-xs text-muted-foreground">{t("Expire le")}{" "}
                    {new Date(token.expires_at * 1000).toLocaleDateString(
                      getLocale(),
                    )}</p>
                  </div>
                  <Button
                    variant="outline"
                    disabled={busy}
                    onClick={() =>
                      void action(async () => {
                        await request(`/auth/tokens/${token.id}`, {
                          method: "DELETE",
                        });
                        setSecret("");
                      })
                    }
                  >
                    {t("Révoquer {name}", {name: token.name})}
                  </Button>
                </li>
              ))}
            </ul>
          </div>
        </CardContent>
      </Card>}
      {view === "access" && user.role === "admin" && (
        <Card role="region" aria-labelledby="account-users-title">
          <CardHeader>
            <CardTitle><h2 id="account-users-title">{t("Administration des utilisateurs")}</h2></CardTitle>
            <CardDescription>
              {t("Les rôles OIDC proviennent des groupes Keycloak. La désactivation d’un compte révoque ses accès Cold VTL.")} </CardDescription>
          </CardHeader>
          <CardContent className="space-y-5">
            <h3 className="text-sm font-medium">{t("Créer un compte local")}</h3>
            <form
              className="grid items-end gap-4 sm:grid-cols-2 xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto_auto]"
              onSubmit={(e) =>
                submit(e, async (f) => {
                  await post("/auth/users", {
                    username: f.get("username"),
                    password: f.get("password"),
                    role: f.get("role"),
                  });
                })
              }
            >
              <div className="space-y-2">
                <Label htmlFor="new-user">{t("Nouvel utilisateur local")}</Label>
                <Input
                  id="new-user"
                  name="username"
                  required
                  minLength={3}
                  maxLength={64}
                  pattern="[a-z0-9._\-]+"
                  autoComplete="off"
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="new-user-password">{t("Mot de passe initial")}</Label>
                <Input
                  id="new-user-password"
                  name="password"
                  type="password"
                  required
                  minLength={12}
                  maxLength={1024}
                  autoComplete="new-password"
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="new-user-role">{t("Rôle du nouvel utilisateur")}</Label>
                <Select id="new-user-role" name="role" defaultValue="viewer">
                  <SelectOption value="viewer">
                    {t("Lecture seule")} </SelectOption>
                  <SelectOption value="admin">
                    {t("Administrateur")} </SelectOption>
                </Select>
              </div>
              <Button disabled={busy}>{t("Créer l’utilisateur")}</Button>
            </form>
            <div className="space-y-3 border-t pt-5">
              <h3 className="text-sm font-medium">{t("Comptes existants")}</h3>
              <ul className="divide-y">
                {users.map((item) => (
                  <li
                    key={item.id}
                    className="flex flex-wrap items-center justify-between gap-3 py-3 first:pt-0 last:pb-0"
                  >
                    <div className="min-w-0 text-sm">
                      <p className="break-words font-medium">{item.username}</p>
                      <p className="text-xs text-muted-foreground">{item.issuer ? t("Compte OIDC") : t("Compte local")}{item.disabled ? t(" · Désactivé") : ""}</p>
                    </div>
                    <div className="flex flex-wrap items-center gap-2">
                      <Select
                        aria-label={t("Rôle de {name}", {name: item.username})}
                        value={item.role}
                        disabled={busy || !!item.issuer || item.id === user.id}
                        onValueChange={(selectedValue) => void action(async () => {
                            await post(`/auth/users/${item.id}`, {
                              role: selectedValue,
                              disabled: item.disabled,
                            });
                          })}
                      >
                        <SelectOption value="viewer">
                          {t("Lecture seule")} </SelectOption>
                        <SelectOption value="admin">
                          {t("Administrateur")} </SelectOption>
                      </Select>
                      <Button
                        variant="outline"
                        disabled={busy || item.id === user.id}
                        onClick={() =>
                          void action(async () => {
                            await post(`/auth/users/${item.id}`, {
                              role: item.role,
                              disabled: !item.disabled,
                            });
                          })
                        }
                      >
                        {item.disabled ? t("Réactiver") : t("Désactiver")}
                      </Button>
                    </div>
                  </li>
                ))}
              </ul>
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  );
}
