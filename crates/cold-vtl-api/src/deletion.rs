use crate::{ApiError, AppState, cartridges};
use axum::{Json, extract::State};
use cold_vtl_core::Barcode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    barcodes: Vec<String>,
}
impl Selection {
    fn validate(&self) -> Result<(), ApiError> {
        if self.barcodes.is_empty()
            || self.barcodes.len() > 64
            || self
                .barcodes
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.barcodes.len()
        {
            return Err(ApiError::bad_request("Choose 1 to 64 distinct cartridges."));
        }
        for value in &self.barcodes {
            Barcode::parse(value.clone()).map_err(ApiError::bad_request)?;
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Delete {
    request_id: String,
    barcodes: Vec<String>,
    confirmation: String,
}
impl Delete {
    fn validate(&self) -> Result<(), ApiError> {
        Selection {
            barcodes: self.barcodes.clone(),
        }
        .validate()?;
        if self.confirmation != "delete-local"
            || self.request_id.len() != 32
            || !self
                .request_id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(ApiError::bad_request(
                "Invalid local deletion confirmation.",
            ));
        }
        Ok(())
    }
}

pub async fn options(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let id = state.catalog.vtl_id().await?.to_string();
    let state = crate::vtls::installed_state(state, id).await?;
    if state.config.cartridge_helper.is_none() {
        return Ok(Json(
            json!({"ready":false,"pending":null,"reason":"The cartridge controller is not configured."}),
        ));
    }
    let mut value = cartridges::invoke(&state, "deletion-options", json!({})).await?;
    if let Some(pending) = value.get_mut("pending").and_then(Value::as_object_mut) {
        if pending.remove("vtl_id") != Some(json!(state.catalog.vtl_id().await?.to_string())) {
            return Err(ApiError::conflict("Deletion belongs to another VTL."));
        }
        serde_json::from_value::<Delete>(json!(pending))
            .map_err(ApiError::conflict)?
            .validate()?;
    }
    Ok(Json(value))
}

pub async fn preview(
    State(state): State<AppState>,
    Json(selection): Json<Selection>,
) -> Result<Json<Value>, ApiError> {
    selection.validate()?;
    let vtl = state.catalog.vtl_id().await?.to_string();
    let Json(mut result) = options(State(state.clone())).await?;
    let mut tapes = vec![];
    for barcode in &selection.barcodes {
        let code = Barcode::parse(barcode.clone()).map_err(ApiError::bad_request)?;
        if state.catalog.cartridge(&code).await?.is_none() {
            return Err(ApiError::not_found("Cartridge not found in this VTL."));
        }
        // Include inactive destinations too. Return only public identity fields,
        // never the destination configuration containing credentials.
        let rows = sqlx::query("SELECT g.destination_id,g.generation,g.checked_at,d.identity FROM remote_generations g JOIN storage_destinations d ON d.vtl_id=g.vtl_id AND d.id=g.destination_id WHERE g.vtl_id=? AND g.barcode=? ORDER BY g.destination_id,g.generation DESC")
            .bind(&vtl).bind(barcode).fetch_all(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
        let exports: Vec<Value> = rows
            .iter()
            .map(|row| {
                let identity = row
                    .get::<Option<String>, _>("identity")
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok());
                json!({"destination_id":row.get::<String,_>("destination_id"),
                   "bucket":identity.and_then(|v| v["bucket"].as_str().map(str::to_owned)),
                   "generation":row.get::<i64,_>("generation"),
                   "checked_at":row.get::<String,_>("checked_at")})
            })
            .collect();
        tapes.push(json!({"barcode":barcode,"exports":exports}));
    }
    result["cartridges"] = json!(tapes);
    Ok(Json(result))
}

pub async fn delete(
    State(state): State<AppState>,
    Json(request): Json<Delete>,
) -> Result<Json<Value>, ApiError> {
    request.validate()?;
    let vtl = state.catalog.vtl_id().await?.to_string();
    let state = crate::vtls::installed_state(state, vtl.clone()).await?;
    let _lock = state
        .operations
        .try_lock()
        .map_err(|_| ApiError::conflict("A VTL operation is already in progress."))?;
    cartridges::idle(&state).await?;
    let mut expected = serde_json::to_value(&request).map_err(ApiError::bad_request)?;
    let result = cartridges::invoke(&state, "delete-local", expected.clone()).await?;
    expected["vtl_id"] = json!(vtl);
    if result["phase"] != "completed" || result["request"] != expected {
        return Err(ApiError::conflict(
            "Deletion not confirmed. Resume the original request.",
        ));
    }
    for barcode in &request.barcodes {
        require_deleted(&state, barcode).await?;
    }
    Ok(Json(
        json!({"phase":"completed","request_id":request.request_id,"barcodes":request.barcodes}),
    ))
}

async fn require_deleted(state: &AppState, barcode: &str) -> Result<(), ApiError> {
    if !state.catalog.local_deleted(barcode).await? {
        return Err(ApiError::conflict(
            "Deletion not confirmed in the catalogue.",
        ));
    }
    Ok(())
}

pub async fn require_local(state: &AppState, barcode: &str) -> Result<(), ApiError> {
    if state.catalog.local_deleted(barcode).await? {
        return Err(ApiError::conflict(
            "This cartridge was deleted locally. Its S3 copies remain viewable and can be restored to a separate folder.",
        ));
    }
    Ok(())
}

pub async fn no_pending(state: &AppState) -> Result<(), ApiError> {
    let root = state.catalog.state_dir();
    let root = if root.join("storage-isolated").exists() {
        root.join("vtls")
            .join(state.catalog.vtl_id().await?.to_string())
    } else {
        root.to_owned()
    };
    if root
        .join("cartridge-deletion-pending.json")
        .try_exists()
        .map_err(ApiError::conflict)?
    {
        return Err(ApiError::conflict(
            "Resume the interrupted local deletion from the library.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deletion_rejects_duplicates_paths_missing_confirmation_and_extra_fields() {
        let good = json!({"request_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","barcodes":["TAPE001"],"confirmation":"delete-local"});
        assert!(
            serde_json::from_value::<Delete>(good.clone())
                .unwrap()
                .validate()
                .is_ok()
        );
        for (key, value) in [
            ("barcodes", json!([])),
            ("barcodes", json!(["TAPE001", "TAPE001"])),
            ("barcodes", json!(["../tapes"])),
            ("confirmation", json!("delete-s3")),
            ("request_id", json!("../journal")),
        ] {
            let mut input = good.clone();
            input[key] = value;
            assert!(
                serde_json::from_value::<Delete>(input)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        let mut extra = good;
        extra["media_root"] = json!("/other-vtl");
        assert!(serde_json::from_value::<Delete>(extra).is_err());
    }

    #[tokio::test]
    async fn preview_includes_inactive_s3_destinations_and_rejects_foreign_tapes() {
        let root = std::env::temp_dir().join(format!("delete-preview-{}", uuid::Uuid::new_v4()));
        let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
            .await
            .unwrap();
        let tape = cold_vtl_core::Cartridge::new(Barcode::parse("TAPE001").unwrap(), 1024).unwrap();
        catalog.create_cartridge(&tape).await.unwrap();
        let vtl = catalog.vtl_id().await.unwrap().to_string();
        let active = catalog.destination().await.unwrap().id;
        sqlx::query("INSERT INTO storage_destinations(id,vtl_id,identity,active) VALUES('old',?,'{\"bucket\":\"archives-2025\"}',0)")
            .bind(&vtl).execute(&catalog.pool()).await.unwrap();
        for destination in [&active, "old"] {
            sqlx::query("INSERT INTO remote_generations(vtl_id,destination_id,barcode,generation,metadata,cartridge_id) SELECT vtl_id,?,barcode,7,'{}',id FROM cartridges WHERE vtl_id=? AND barcode='TAPE001'")
                .bind(destination).bind(&vtl).execute(&catalog.pool()).await.unwrap();
        }
        let state = AppState {
            catalog,
            operations: std::sync::Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: std::sync::Arc::new(crate::AppConfig {
                storage_socket: None,
                media_root: root.join("media"),
                restore_root: root.join("restore"),
                worker_path: root.join("worker"),
                maintenance_helper: None,
                cartridge_helper: None,
                quiesce_check: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        };
        let Json(value) = preview(
            State(state.clone()),
            Json(Selection {
                barcodes: vec!["TAPE001".into()],
            }),
        )
        .await
        .unwrap();
        let exports = value["cartridges"][0]["exports"].as_array().unwrap();
        assert_eq!(exports.len(), 2);
        assert!(
            exports
                .iter()
                .any(|item| item["bucket"] == "archives-2025" && item["generation"] == 7)
        );
        assert!(
            preview(
                State(state.clone()),
                Json(Selection {
                    barcodes: vec!["FOREIGN1".into()]
                })
            )
            .await
            .is_err()
        );
        sqlx::query(
            "INSERT INTO cartridge_deletions(vtl_id,barcode,request_id) VALUES(?,'TAPE001','a')",
        )
        .bind(vtl)
        .execute(&state.catalog.pool())
        .await
        .unwrap();
        assert!(require_local(&state, "TAPE001").await.is_err());
        let Json(after) = preview(
            State(state.clone()),
            Json(Selection {
                barcodes: vec!["TAPE001".into()],
            }),
        )
        .await
        .unwrap();
        assert_eq!(after["cartridges"], value["cartridges"]);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }
}
