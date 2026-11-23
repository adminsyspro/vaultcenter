use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
#[derive(Serialize)]
pub struct Mail {
    pub subject: String,
    pub text: String,
    pub html: String,
    pub logo_base64: String,
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn field(event: &Value, key: &str) -> String {
    event[key]
        .as_str()
        .unwrap_or("—")
        .chars()
        .filter(|c| !c.is_control())
        .take(180)
        .collect()
}
pub fn render(settings: &Settings, event: &Value, base: &str) -> Mail {
    // Test-event labels are generated copy, and follow the language at send time
    // even when the test was queued before a settings change.
    let display_field = |key: &str| -> String {
        if event["kind"] == "test" {
            match key {
                "subject" => return settings.localized("Email de test", "Test email").into(),
                "vtl_name" => {
                    return settings
                        .localized("Configuration globale", "Global configuration")
                        .into();
                }
                "occurred_at" => {
                    return settings
                        .localized(
                            "Test demandé dans VaultCenter",
                            "Test requested in VaultCenter",
                        )
                        .into();
                }
                _ => {}
            }
        }
        field(event, key)
    };
    let operation = match event["kind"].as_str().unwrap_or("") {
        "export" => settings.localized("Export S3", "S3 export"),
        "restore" => settings.localized("Restauration S3", "S3 restore"),
        "reintegrate" => settings.localized("Réintégration PBS", "PBS reintegration"),
        "archive" | "archive_submit" => settings.localized("Archivage", "Archiving"),
        "archive_prepare" => settings.localized("Préparation d’archivage", "Archive preparation"),
        "recall" | "recall_submit" => settings.localized("Rappel", "Archive recall"),
        "recall_prepare" => settings.localized("Préparation du rappel", "Recall preparation"),
        "test" => settings.localized("Email de test", "Test email"),
        _ => settings.localized("Opération", "Operation"),
    };
    let (status, color, summary) = match event["outcome"].as_str() {
        Some("success") => (
            settings.localized("Terminé", "Completed"),
            "#0f766e",
            settings.localized("L’opération est terminée. Consultez VaultCenter pour son résultat détaillé.", "The operation is complete. Open VaultCenter for the detailed result."),
        ),
        Some("uncertain") => (
            settings.localized("Résultat incertain", "Uncertain result"),
            "#ff6a00",
            settings.localized("Le résultat n’a pas pu être confirmé. Vérifiez l’état dans VaultCenter avant de relancer une action.", "The result could not be confirmed. Check the status in VaultCenter before retrying an action."),
        ),
        _ => (
            settings.localized("Échec", "Failed"),
            "#be123c",
            settings.localized("L’opération n’a pas abouti. Consultez son détail dans VaultCenter pour identifier la cause.", "The operation failed. Open its details in VaultCenter to identify the cause."),
        ),
    };
    let summary = if event["kind"] == "test" {
        settings.localized("Ce message confirme que le relais SMTP a accepté l’email de test de votre installation VaultCenter.", "This message confirms that the SMTP relay accepted the test email from your VaultCenter installation.")
    } else {
        summary
    };
    let subject = format!(
        "[VaultCenter · {}] {operation} · {status} · {}",
        settings.installation,
        display_field("subject")
    );
    let mut url = url::Url::parse(base)
        .ok()
        .filter(|u| {
            matches!(u.scheme(), "https" | "http")
                && u.username().is_empty()
                && u.password().is_none()
        })
        .unwrap_or_else(|| url::Url::parse("https://localhost:8443").unwrap());
    url.set_query(None);
    url.set_path("/");
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("vtl", event["vtl_id"].as_str().unwrap_or(""))
        .append_pair("job", event["job_id"].as_str().unwrap_or(""))
        .finish();
    url.set_fragment(Some(&format!("jobs?{query}")));
    let mut rows = String::new();
    let mut text = format!(
        "VaultCenter · {}\n{operation} · {status}\n{summary}\n\n",
        settings.installation
    );
    for (label, key) in [
        (settings.localized("Bibliothèque", "Library"), "vtl_name"),
        (
            settings.localized("Cartouche / bucket", "Cartridge / bucket"),
            "subject",
        ),
        (settings.localized("Bucket S3", "S3 bucket"), "bucket"),
        (
            settings.localized("Destination", "Destination"),
            "destination_id",
        ),
        (
            settings.localized("Date (UTC)", "Date (UTC)"),
            "occurred_at",
        ),
        (settings.localized("Opération", "Operation"), "job_id"),
    ] {
        if ["bucket", "destination_id"].contains(&key) && event[key].as_str().is_none() {
            continue;
        }
        let value = display_field(key);
        if value.is_empty() {
            continue;
        }
        text.push_str(&format!("{label} : {value}\n"));
        rows.push_str(&format!("<tr><td style=\"padding:12px 12px 12px 0;border-bottom:1px solid #dedede;color:#616161;width:38%;vertical-align:top\">{label}</td><td style=\"padding:12px 0;border-bottom:1px solid #dedede;color:#202020;overflow-wrap:anywhere\">{}</td></tr>",escape(&value)));
    }
    let action = settings.localized("Voir l’opération", "View operation");
    let text_footer = settings.localized(
        "Notification automatique de VaultCenter. Ne pas répondre.",
        "Automatic notification from VaultCenter. Do not reply.",
    );
    text.push_str(&format!("\n{action} : {url}\n{text_footer}"));
    let footer = settings.localized(
        "Notification automatique de votre console de stockage VaultCenter.",
        "Automatic notification from your VaultCenter storage console.",
    );
    let footer_help = settings.localized(
        "Connectez-vous à l’application pour consulter les détails. Ne pas répondre.",
        "Sign in to the application to view details. Do not reply.",
    );
    let language = settings.localized("fr", "en");
    let logo_base64 = STANDARD.encode(include_bytes!(
        "../../../../ui/public/brand/vaultcenter.png"
    ));
    let html = format!(
        r#"<!doctype html><html lang="{language}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{title}</title></head><body style="margin:0;background:#f3f3f3;color:#202020;font-family:Manrope,Arial,Helvetica,sans-serif"><table role="presentation" width="100%" cellspacing="0" cellpadding="0"><tr><td align="center" style="padding:24px 12px"><table role="presentation" width="100%" cellspacing="0" cellpadding="0" style="max-width:600px;background:#ffffff;border:1px solid #dedede"><tr><td style="padding:24px;background:#111827;border-top:4px solid #ff6a00;color:#ffffff"><table role="presentation" cellspacing="0" cellpadding="0"><tr><td style="padding-right:14px;vertical-align:middle"><img src="data:image/png;base64,{logo_base64}" width="48" height="48" alt="" style="display:block;border:0"></td><td style="vertical-align:middle"><span style="font-size:26px;font-weight:700">VaultCenter</span><br><span style="font-size:14px;color:#e4e4e4">{installation}</span></td></tr></table></td></tr><tr><td style="padding:28px 24px"><p style="margin:0 0 10px;color:{color};font-size:14px;font-weight:700">{status}</p><h1 style="margin:0 0 16px;font-size:26px;line-height:1.25">{operation}</h1><p style="font-size:16px;line-height:1.6;margin:0 0 20px">{summary}</p><table role="presentation" width="100%" cellspacing="0" cellpadding="0" style="font-size:14px;line-height:1.5;table-layout:fixed">{rows}</table><p style="margin:28px 0 0"><a href="{url}" style="display:inline-block;padding:14px 20px;background:#ff6a00;color:#111827;text-decoration:none;font-size:15px;font-weight:700">{action}</a></p></td></tr><tr><td style="padding:20px 24px;background:#f7f7f7;font-size:12px;line-height:1.6;color:#616161">{footer}<br>{footer_help}</td></tr></table></td></tr></table></body></html>"#,
        title = escape(&subject),
        installation = escape(&settings.installation),
        url = escape(url.as_str())
    );
    Mail {
        subject,
        text,
        html,
        logo_base64,
    }
}
