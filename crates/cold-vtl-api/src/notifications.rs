mod delivery;
mod template;
#[cfg(test)]
mod tests;

use crate::{ApiError, AppState, auth::Principal};
use axum::{
    Extension, Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool};
use std::collections::BTreeMap;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    enabled: bool,
    language: String,
    installation: String,
    host: String,
    port: u16,
    tls: String,
    username: String,
    #[serde(skip)]
    password: String,
    sender: String,
    sender_name: String,
    recipients: Vec<String>,
    events: BTreeMap<String, Filter>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Filter {
    success: bool,
    failure: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            language: "fr".into(),
            installation: "VaultCenter".into(),
            host: String::new(),
            port: 587,
            tls: "starttls".into(),
            username: String::new(),
            password: String::new(),
            sender: String::new(),
            sender_name: "VaultCenter".into(),
            recipients: vec![],
            events: ["export", "restore", "reintegrate", "archive", "recall"]
                .into_iter()
                .map(|k| {
                    (
                        k.into(),
                        Filter {
                            success: true,
                            failure: true,
                        },
                    )
                })
                .collect(),
        }
    }
}
fn invalid() -> ApiError {
    ApiError::bad_request(
        "Invalid email configuration: check the server, TLS, addresses and required fields.",
    )
}
fn safe_text(value: &str, max: usize) -> bool {
    value.len() <= max && !value.chars().any(char::is_control)
}
fn email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && value.len() <= 254
        && !domain.is_empty()
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain.contains('.')
        && local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&b))
        && domain
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !value.contains("..")
}
impl Settings {
    fn localized<'a>(&self, french: &'a str, english: &'a str) -> &'a str {
        if self.language == "en" {
            english
        } else {
            french
        }
    }
    fn validate(&self) -> Result<(), ApiError> {
        if !["fr", "en"].contains(&self.language.as_str())
            || !safe_text(&self.installation, 100)
            || self.installation.trim().is_empty()
            || !safe_text(&self.sender_name, 100)
            || self.sender_name.trim().is_empty()
            || !safe_text(&self.username, 254)
            || self.password.len() > 4096
            || !["starttls", "implicit"].contains(&self.tls.as_str())
            || self.port == 0
            || self.host.len() > 253
            || self
                .host
                .chars()
                .any(|c| !c.is_ascii_alphanumeric() && !".-:".contains(c))
            || self.recipients.len() > 20
            || self.recipients.iter().any(|s| !email(s))
            || (!self.sender.is_empty() && !email(&self.sender))
            || self.events.keys().any(|s| {
                !["export", "restore", "reintegrate", "archive", "recall"].contains(&s.as_str())
            })
            || (self.enabled
                && (self.host.is_empty() || self.sender.is_empty() || self.recipients.is_empty()))
            || (!self.username.is_empty() && self.password.is_empty())
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    revision: i64,
    settings: Settings,
    password: Option<String>,
    #[serde(default)]
    clear_password: bool,
}
async fn load(pool: &SqlitePool) -> Result<(i64, Settings), ApiError> {
    let (revision, raw): (i64, String) =
        sqlx::query_as("SELECT revision,configuration FROM notification_settings WHERE id=1")
            .fetch_one(pool)
            .await
            .map_err(ApiError::conflict)?;
    let mut value: Value = serde_json::from_str(&raw).map_err(|_| invalid())?;
    let password = value
        .as_object_mut()
        .and_then(|o| o.remove("password"))
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let mut settings: Settings = serde_json::from_value(value).map_err(|_| invalid())?;
    settings.password = password;
    Ok((revision, settings))
}
fn public_settings(revision: i64, settings: &Settings) -> Value {
    json!({"revision":revision,"settings":settings,"has_password":!settings.password.is_empty()})
}
async fn store(pool: &SqlitePool, mut update: Update) -> Result<Value, ApiError> {
    let (revision, previous) = load(pool).await?;
    if revision != update.revision {
        return Err(ApiError::conflict(
            "Configuration changed elsewhere. Reload before saving.",
        ));
    }
    if update.clear_password && update.password.is_some() {
        return Err(invalid());
    }
    update.settings.password = if update.clear_password {
        String::new()
    } else {
        update.password.unwrap_or(previous.password)
    };
    update.settings.recipients.sort();
    update.settings.recipients.dedup();
    update.settings.validate()?;
    let mut value = serde_json::to_value(&update.settings).map_err(|_| invalid())?;
    value["password"] = json!(update.settings.password);
    let result=sqlx::query("UPDATE notification_settings SET revision=revision+1,configuration=? WHERE id=1 AND revision=?").bind(value.to_string()).bind(revision).execute(pool).await.map_err(|_|ApiError::conflict("Unable to save notification settings."))?;
    if result.rows_affected() != 1 {
        return Err(ApiError::conflict(
            "Configuration changed elsewhere. Reload before saving.",
        ));
    }
    Ok(public_settings(revision + 1, &update.settings))
}
fn admin(principal: &Principal) -> Result<(), ApiError> {
    if principal.user.role != "admin" {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            message: "administrator required".into(),
        });
    }
    Ok(())
}
pub async fn get(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let (r, s) = load(&state.catalog.pool()).await?;
    Ok(Json(public_settings(r, &s)))
}
pub async fn put(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(update): Json<Update>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    Ok(Json(store(&state.catalog.pool(), update).await?))
}
fn public_url() -> String {
    std::env::var("COLD_VTL_PUBLIC_URL").unwrap_or_else(|_| "https://localhost:8443".into())
}
pub async fn preview(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let (_, settings) = load(&state.catalog.pool()).await?;
    let event = json!({"kind":"export","outcome":"success","subject":settings.localized("CVT008L8 · exemple", "CVT008L8 · example"),"vtl_name":settings.localized("Bibliothèque de démonstration", "Demo library"),"occurred_at":settings.localized("Exemple de notification", "Example notification"),"job_id":"","vtl_id":""});
    Ok(Json(
        serde_json::to_value(template::render(&settings, &event, &public_url())).unwrap(),
    ))
}
pub async fn test(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let pool = state.catalog.pool();
    let (_, settings) = load(&pool).await?;
    settings.validate()?;
    let mut tx = pool.begin().await.map_err(ApiError::conflict)?;
    if !settings.enabled {
        return Err(ApiError::conflict(
            "Save and enable notifications before testing.",
        ));
    }
    // Rate limit is atomic with insertion, including concurrent requests.
    let id = uuid::Uuid::new_v4().to_string();
    let payload = json!({"kind":"test","outcome":"success","subject":settings.localized("Email de test", "Test email"),"vtl_name":settings.localized("Configuration globale", "Global configuration"),"occurred_at":settings.localized("Test demandé dans VaultCenter", "Test requested in VaultCenter"),"job_id":"","vtl_id":""});
    let result=sqlx::query("INSERT INTO notification_events(id,payload,recipients) SELECT ?,?,json_extract(configuration,'$.recipients') FROM notification_settings WHERE id=1 AND json_extract(configuration,'$.enabled')=1 AND NOT EXISTS(SELECT 1 FROM notification_events WHERE json_extract(payload,'$.kind')='test' AND created_at>unixepoch()-60)").bind(&id).bind(payload.to_string()).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    if result.rows_affected() != 1 {
        return Err(ApiError::conflict(
            "A test was already requested in the last minute, or notifications have been disabled.",
        ));
    }
    tx.commit().await.map_err(ApiError::conflict)?;
    Ok(Json(json!({"status":"queued","id":id})))
}
#[derive(Deserialize)]
pub struct Page {
    #[serde(default)]
    offset: u32,
}
pub async fn deliveries(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let rows=sqlx::query("SELECT d.*,e.payload,e.created_at FROM notification_deliveries d JOIN notification_events e ON e.id=d.event_id ORDER BY e.created_at DESC,d.id LIMIT 26 OFFSET ?").bind(page.offset).fetch_all(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    let items:Vec<Value>=rows.iter().take(25).map(|r|json!({"id":r.get::<String,_>("id"),"recipient":r.get::<String,_>("recipient"),"status":r.get::<String,_>("status"),"attempts":r.get::<i64,_>("attempts"),"category":r.get::<Option<String>,_>("category"),"created_at":r.get::<i64,_>("created_at"),"event":serde_json::from_str::<Value>(&r.get::<String,_>("payload")).unwrap_or(Value::Null)})).collect();
    Ok(Json(
        json!({"items":items,"has_more":rows.len()>25,"offset":page.offset}),
    ))
}
pub fn monitor(state: AppState) {
    delivery::monitor(state, public_url());
}
