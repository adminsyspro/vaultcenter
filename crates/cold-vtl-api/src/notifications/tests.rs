use super::*;

fn configured() -> Settings {
    Settings {
        enabled: true,
        host: "smtp.example.test".into(),
        sender: "vaultcenter@example.test".into(),
        recipients: vec!["ops@example.test".into()],
        ..Settings::default()
    }
}
#[test]
fn validates_transport_headers_and_recipients() {
    let mut s = configured();
    assert!(s.validate().is_ok());
    s.sender_name = "x\r\nBcc: victim@example.test".into();
    assert!(s.validate().is_err());
    s.sender_name = "VaultCenter".into();
    s.tls = "none".into();
    assert!(s.validate().is_err());
    s.tls = "starttls".into();
    s.recipients = vec!["bad@".into()];
    assert!(s.validate().is_err());
}
#[test]
fn renderer_escapes_untrusted_names_and_never_copies_raw_errors() {
    let event = json!({"kind":"export","outcome":"failure","subject":"<script>alert(1)</script>","vtl_name":"<b>Lab</b>","message":"SECRET","job_id":"job-1","vtl_id":"vtl-1","occurred_at":"2026-10-04T09:00:00Z"});
    let mail = template::render(&configured(), &event, "https://vaultcenter.example.test");
    assert!(!mail.html.contains("<script>"));
    assert!(!mail.html.contains("SECRET"));
    assert!(mail.html.contains("&lt;b&gt;Lab&lt;/b&gt;"));
    assert!(mail.text.contains("Échec"));
    assert!(mail.html.contains("#jobs?vtl=vtl-1"));
}
#[tokio::test]
async fn configuration_preserves_secret_and_rejects_stale_revision() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let mut s = configured();
    s.username = "account".into();
    s.language = "en".into();
    let update = Update {
        revision: 0,
        settings: s.clone(),
        password: Some("secret".into()),
        clear_password: false,
    };
    store(&pool, update).await.unwrap();
    assert!(
        store(
            &pool,
            Update {
                revision: 0,
                settings: s.clone(),
                password: None,
                clear_password: false
            }
        )
        .await
        .is_err()
    );
    store(
        &pool,
        Update {
            revision: 1,
            settings: s,
            password: None,
            clear_password: false,
        },
    )
    .await
    .unwrap();
    let (revision, s) = load(&pool).await.unwrap();
    assert_eq!(revision, 2);
    assert_eq!(s.password, "secret");
    assert_eq!(s.language, "en");
    assert!(!public_settings(revision, &s).to_string().contains("secret"));
}

#[test]
fn email_language_defaults_to_french_and_rejects_unsupported_values() {
    let legacy: Settings = serde_json::from_value(json!({"enabled":false})).unwrap();
    assert_eq!(legacy.language, "fr");
    assert!(legacy.validate().is_ok());
    for language in ["fr", "en"] {
        let settings: Settings = serde_json::from_value(json!({"language":language})).unwrap();
        assert!(settings.validate().is_ok());
    }
    for language in ["", "de", "en-GB", "FR"] {
        let settings: Settings = serde_json::from_value(json!({"language":language})).unwrap();
        assert!(settings.validate().is_err(), "accepted {language}");
    }
}

#[test]
fn renders_both_languages_without_translating_event_identifiers() {
    let event = json!({"kind":"restore","outcome":"success","subject":"FR-NAME-L8","vtl_name":"Bibliothèque personnelle","job_id":"job-1","vtl_id":"vtl-1","occurred_at":"2026-10-07T09:00:00Z"});
    let mut settings = configured();
    let french = template::render(&settings, &event, "https://vaultcenter.example.test");
    assert!(
        french
            .subject
            .contains("Restauration S3 · Terminé · FR-NAME-L8")
    );
    assert!(french.html.contains("<html lang=\"fr\">"));
    assert!(
        french
            .text
            .contains("Bibliothèque : Bibliothèque personnelle")
    );
    assert!(french.text.contains("Voir l’opération :"));
    settings.language = "en".into();
    let english = template::render(&settings, &event, "https://vaultcenter.example.test");
    assert!(
        english
            .subject
            .contains("S3 restore · Completed · FR-NAME-L8")
    );
    assert!(english.html.contains("<html lang=\"en\">"));
    assert!(english.text.contains("Library : Bibliothèque personnelle"));
    assert!(english.text.contains("View operation :"));
    assert!(english.html.contains("Do not reply."));
    assert!(!english.html.contains("Voir l’opération"));
}

#[test]
fn english_email_escapes_data_and_covers_failure_uncertainty_and_queued_tests() {
    let mut settings = configured();
    settings.language = "en".into();
    settings.installation = "<img src=x>".into();
    for (outcome, expected) in [("failure", "Failed"), ("uncertain", "Uncertain result")] {
        let event = json!({"kind":"archive","outcome":outcome,"subject":"<script>x</script>","vtl_name":"<b>Lab</b>","message":"SECRET","job_id":"job-1","vtl_id":"vtl-1"});
        let mail = template::render(&settings, &event, "https://vaultcenter.example.test");
        assert!(mail.subject.contains(expected));
        assert!(!mail.html.contains("<script>"));
        assert!(mail.html.contains("&lt;script&gt;"));
        assert!(mail.html.contains("&lt;img src=x&gt;"));
        assert!(!mail.html.contains("SECRET"));
    }
    let event = json!({"kind":"test","outcome":"success","subject":"Email de test","vtl_name":"Configuration globale","occurred_at":"Test demandé dans VaultCenter"});
    let mail = template::render(&settings, &event, "https://vaultcenter.example.test");
    assert!(mail.subject.contains("Test email"));
    assert!(mail.text.contains("Global configuration"));
    assert!(mail.text.contains("Test requested in VaultCenter"));
    assert!(!mail.text.contains("Email de test"));
}
