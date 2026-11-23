use super::*;
use std::{process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

async fn expand(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let rows=sqlx::query("SELECT id,recipients FROM notification_events WHERE expanded=0 ORDER BY created_at LIMIT 100").fetch_all(&mut *tx).await?;
    for row in rows {
        let id: String = row.get("id");
        let recipients: Vec<String> =
            serde_json::from_str(&row.get::<String, _>("recipients")).unwrap_or_default();
        for recipient in recipients {
            sqlx::query("INSERT OR IGNORE INTO notification_deliveries(id,event_id,recipient) VALUES(?,?,?)").bind(uuid::Uuid::new_v4().to_string()).bind(&id).bind(recipient).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE notification_events SET expanded=1 WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}
async fn recover(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE notification_deliveries SET status='uncertain',category='interrupted',updated_at=unixepoch() WHERE status='sending'").execute(pool).await?;
    Ok(())
}
async fn recover_expired(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE notification_deliveries SET status='uncertain',category='interrupted',updated_at=unixepoch() WHERE status='sending' AND updated_at<unixepoch()-120").execute(pool).await?;
    Ok(())
}
async fn transport(
    helper: &str,
    settings: &Settings,
    recipient: &str,
    id: &str,
    mail: template::Mail,
) -> Value {
    let input = json!({"host":settings.host,"port":settings.port,"tls":settings.tls,"username":settings.username,"password":settings.password,"sender":settings.sender,"sender_name":settings.sender_name,"recipient":recipient,"subject":mail.subject,"text":mail.text,"html":mail.html,"logo_base64":mail.logo_base64,"message_id":format!("<{id}@vaultcenter.invalid>")});
    let mut child = match Command::new(helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return json!({"status":"failed","category":"helper_unavailable"}),
    };
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .await?;
        child.wait_with_output().await
    })
    .await;
    match result {
        Ok(Ok(output)) if output.status.success() => serde_json::from_slice(&output.stdout)
            .unwrap_or(json!({"status":"uncertain","category":"invalid_helper_result"})),
        _ => json!({"status":"uncertain","category":"interrupted"}),
    }
}
async fn tick(pool: &SqlitePool, helper: &str, base: &str) -> Result<(), ApiError> {
    recover_expired(pool).await.map_err(ApiError::conflict)?;
    expand(pool).await.map_err(ApiError::conflict)?;
    let (revision, settings) = load(pool).await?;
    if !settings.enabled {
        return Ok(());
    }
    settings.validate()?;
    // Claim only against the configuration revision we validated. Recipients are
    // immutable event snapshots; a settings edit never redirects queued messages.
    let row=sqlx::query("UPDATE notification_deliveries SET status='sending',attempts=attempts+1,updated_at=unixepoch() WHERE id=(SELECT id FROM notification_deliveries WHERE status='queued' AND next_attempt<=unixepoch() ORDER BY next_attempt,id LIMIT 1) AND EXISTS(SELECT 1 FROM notification_settings WHERE id=1 AND revision=? AND json_extract(configuration,'$.enabled')=1) RETURNING id,event_id,recipient,attempts").bind(revision).fetch_optional(pool).await.map_err(ApiError::conflict)?;
    let Some(row) = row else { return Ok(()) };
    let id: String = row.get("id");
    let recipient: String = row.get("recipient");
    let attempts: i64 = row.get("attempts");
    let raw: String = sqlx::query_scalar("SELECT payload FROM notification_events WHERE id=?")
        .bind(row.get::<String, _>("event_id"))
        .fetch_one(pool)
        .await
        .map_err(ApiError::conflict)?;
    let event: Value =
        serde_json::from_str(&raw).map_err(|_| ApiError::conflict("Invalid email event."))?;
    let result = transport(
        helper,
        &settings,
        &recipient,
        &id,
        template::render(&settings, &event, base),
    )
    .await;
    // Retry only the local acknowledgement, never the SMTP transaction.
    for attempt in 0..3 {
        match finish(pool, &id, attempts, &result).await {
            Ok(()) => return Ok(()),
            Err(error) if attempt == 2 => return Err(ApiError::conflict(error)),
            Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
    Ok(())
}
async fn finish(
    pool: &SqlitePool,
    id: &str,
    attempts: i64,
    result: &Value,
) -> Result<(), sqlx::Error> {
    let status = match result["status"].as_str() {
        Some("sent") => "sent",
        Some("failed") => "failed",
        Some("transient") if attempts < 5 => "queued",
        Some("transient") => "failed",
        _ => "uncertain",
    };
    let category = match result["category"].as_str() {
        Some(s)
            if [
                "smtp_accepted",
                "configuration",
                "certificate",
                "authentication",
                "sender_rejected",
                "recipient_rejected",
                "message_rejected",
                "smtp_rejected",
                "connection",
                "helper_unavailable",
                "interrupted",
            ]
            .contains(&s) =>
        {
            s
        }
        _ => "invalid_helper_result",
    };
    let delay = match attempts {
        1 => 60,
        2 => 300,
        3 => 900,
        _ => 3600,
    };
    sqlx::query("UPDATE notification_deliveries SET status=?,category=?,next_attempt=unixepoch()+?,updated_at=unixepoch() WHERE id=? AND status='sending'").bind(status).bind(category).bind(delay).bind(id).execute(pool).await?;
    Ok(())
}
pub(super) fn monitor(state: AppState, base: String) {
    let helper = std::env::var("COLD_VTL_EMAIL_HELPER")
        .unwrap_or_else(|_| "/usr/local/libexec/vaultcenter-notify-email".into());
    tokio::spawn(async move {
        let pool = state.catalog.pool();
        if recover(&pool).await.is_err() {
            eprintln!("VaultCenter email dispatcher recovery unavailable");
            return;
        }
        loop {
            if tick(&pool, &helper, &base).await.is_err() {
                eprintln!("VaultCenter email dispatcher temporarily unavailable");
            }
            let _=sqlx::query("DELETE FROM notification_deliveries WHERE status='sent' AND updated_at<unixepoch()-2592000").execute(&pool).await;
            let _=sqlx::query("DELETE FROM notification_events WHERE expanded=1 AND created_at<unixepoch()-2592000 AND NOT EXISTS(SELECT 1 FROM notification_deliveries d WHERE d.event_id=notification_events.id)").execute(&pool).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn expired_claim_becomes_uncertain_without_touching_live_send() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO notification_events(id,payload,recipients) VALUES('e','{}','[]')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO notification_deliveries(id,event_id,recipient,status,updated_at) VALUES('old','e','a@example.test','sending',unixepoch()-121),('live','e','b@example.test','sending',unixepoch())").execute(&pool).await.unwrap();
        recover_expired(&pool).await.unwrap();
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT id,status FROM notification_deliveries ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![
                ("live".into(), "sending".into()),
                ("old".into(), "uncertain".into())
            ]
        );
    }
    #[tokio::test]
    async fn delivery_survives_restart_and_limits_retries() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO notification_events(id,payload,recipients) VALUES('event','{}','[\"a@example.test\",\"b@example.test\"]')").execute(&pool).await.unwrap();
        expand(&pool).await.unwrap();
        expand(&pool).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM notification_deliveries")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
        let id: String = sqlx::query_scalar("SELECT id FROM notification_deliveries LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE notification_deliveries SET status='sending' WHERE id=?")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        finish(
            &pool,
            &id,
            4,
            &json!({"status":"transient","category":"connection"}),
        )
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM notification_deliveries WHERE id=?"
            )
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            "queued"
        );
        sqlx::query("UPDATE notification_deliveries SET status='sending' WHERE id=?")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        finish(
            &pool,
            &id,
            5,
            &json!({"status":"transient","category":"connection"}),
        )
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM notification_deliveries WHERE id=?"
            )
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            "failed"
        );
        sqlx::query("UPDATE notification_deliveries SET status='sending' WHERE id=?")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        recover(&pool).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM notification_deliveries WHERE id=?"
            )
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            "uncertain"
        );
    }
}
