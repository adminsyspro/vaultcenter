pub mod bandwidth;
mod checksums;
mod limited_body;
use aws_config::BehaviorVersion;
use aws_sdk_s3::{
    Client,
    config::{Credentials, Region},
    primitives::{ByteStream, Length},
    types::{
        CompletedMultipartUpload, CompletedPart, IntelligentTieringAccessTier,
        IntelligentTieringConfiguration, IntelligentTieringStatus, Tiering,
    },
};
use cold_vtl_core::S3Provider;
use std::{collections::HashMap, env, path::Path};
use thiserror::Error;

#[derive(Clone, serde::Deserialize, Eq, PartialEq)]
pub struct S3Config {
    #[serde(default)]
    pub provider: Option<S3Provider>,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    access_key_id: String,
    secret_access_key: String,
}

impl S3Config {
    pub fn provider(&self) -> S3Provider {
        self.provider
            .unwrap_or_else(|| S3Provider::infer(&self.endpoint))
    }

    pub fn validate_provider(&self) -> Result<(), &'static str> {
        self.provider().validate_endpoint(&self.endpoint)
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        let endpoint = required("AWS_ENDPOINT_URL")?;
        let region = env::var("COLD_VTL_S3_REGION")
            .or_else(|_| env::var("AWS_REGION"))
            .or_else(|_| env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| infer_region(&endpoint).to_owned());

        let prefix = env::var("COLD_VTL_PREFIX")
            .unwrap_or_else(|_| "cold-vtl".to_owned())
            .trim_matches('/')
            .to_owned();

        Ok(Self {
            provider: Some(S3Provider::infer(&endpoint)),
            endpoint,
            region,
            bucket: required("COLD_VTL_BUCKET")?,
            prefix,
            access_key_id: required("AWS_ACCESS_KEY_ID")?,
            secret_access_key: required("AWS_SECRET_ACCESS_KEY")?,
        })
    }

    pub fn object_key(&self, suffix: &str) -> String {
        if self.prefix.is_empty() {
            suffix.trim_start_matches('/').to_owned()
        } else {
            format!("{}/{}", self.prefix, suffix.trim_start_matches('/'))
        }
    }
}

#[derive(Clone)]
pub struct S3Store {
    client: Client,
    config: S3Config,
    bandwidth: Option<bandwidth::VtlBandwidth>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredObject {
    pub key: String,
    pub etag: Option<String>,
    pub version_id: Option<String>,
}

pub const MAX_MULTIPART_PARTS: i32 = 10_000;
pub const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
const FILE_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MultipartPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum_sha256: Option<String>,
    pub part_number: i32,
    pub etag: String,
    pub size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BucketStatus {
    pub status: String,
    pub archive_lock_until: Option<String>,
}

#[derive(serde::Serialize)]
pub struct BucketDiscovery {
    pub buckets: Vec<String>,
    pub complete: bool,
    pub error: Option<&'static str>,
}

impl S3Store {
    /// `absent` only on an explicit 404: the one answer proving a bucket was never created.
    pub async fn bucket_presence(&self, bucket: &str) -> &'static str {
        match self.client.head_bucket().bucket(bucket).send().await {
            Ok(_) => "exists",
            Err(error) if error.as_service_error().is_some_and(|e| e.is_not_found()) => "absent",
            Err(_) => "unknown",
        }
    }

    /// Classify credentials with one listing call: `ok`, `list_denied` (valid
    /// keys whose policy forbids listing), `invalid` or `unreachable`.
    pub async fn check_credentials(&self) -> &'static str {
        match self.client.list_buckets().max_buckets(1).send().await {
            Ok(_) => "ok",
            Err(error) => match error.as_service_error().map(|e| e.meta().code()) {
                Some(Some("AccessDenied")) => "list_denied",
                Some(_) => "invalid",
                None => match error.raw_response().map(|r| r.status().as_u16()) {
                    Some(403) => "list_denied",
                    Some(_) => "invalid",
                    None => "unreachable",
                },
            },
        }
    }

    /// Preserve pages already read when permissions or pagination fail.
    /// Never return provider errors, which can contain signed request details.
    pub async fn discover_buckets(&self) -> BucketDiscovery {
        let mut result = BucketDiscovery {
            buckets: Vec::new(),
            complete: false,
            error: None,
        };
        let mut token = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = self
                .client
                .list_buckets()
                .max_buckets(1000)
                .set_continuation_token(token)
                .send()
                .await;
            let Ok(page) = page else {
                result.error =
                    Some("Bucket discovery denied or unavailable; attach a known bucket manually.");
                break;
            };
            result.buckets.extend(
                page.buckets()
                    .iter()
                    .filter_map(|bucket| bucket.name().map(str::to_owned)),
            );
            match page.continuation_token().filter(|s| !s.is_empty()) {
                None => {
                    result.complete = true;
                    break;
                }
                Some(next) if seen.len() < 100 && seen.insert(next.to_owned()) => {
                    token = Some(next.to_owned())
                }
                Some(_) => {
                    result.error = Some("Bucket discovery pagination incomplete.");
                    break;
                }
            }
        }
        result.buckets.sort();
        result.buckets.dedup();
        result
    }

    pub async fn connect(config: S3Config) -> Self {
        let credentials = Credentials::new(
            config.access_key_id.clone(),
            config.secret_access_key.clone(),
            None,
            None,
            "cold-vtl-env",
        );
        let shared = aws_config::defaults(BehaviorVersion::latest())
            .credentials_provider(credentials)
            .region(Region::new(config.region.clone()))
            .endpoint_url(&config.endpoint)
            .load()
            .await;
        let service_config = aws_sdk_s3::config::Builder::from(&shared)
            .force_path_style(true)
            .build();

        Self {
            client: Client::from_conf(service_config),
            config,
            bandwidth: None,
        }
    }

    pub fn with_bandwidth(mut self, limiter: bandwidth::VtlBandwidth) -> Self {
        self.bandwidth = Some(limiter);
        self
    }

    fn object_body(&self, body: ByteStream) -> ByteStream {
        match &self.bandwidth {
            Some(limiter) => limited_body::limit(body, limiter.clone()),
            None => body,
        }
    }

    /// Create only after a definite absence. This cloud mutation is never retried.
    pub async fn create_empty_ovh_bucket(&self, bucket: &str) -> Result<(), S3Error> {
        if !cold_vtl_core::valid_managed_bucket_name(bucket) {
            return Err(S3Error::Request("invalid new bucket name".into()));
        }
        if self.config.provider() != cold_vtl_core::S3Provider::OvhColdArchiveV1 {
            return Err(S3Error::Request(
                "bucket creation requires OVH Cold Archive v1".into(),
            ));
        }
        // Every failure before the PUT is definitive: no bucket can exist yet.
        match self.client.head_bucket().bucket(bucket).send().await {
            Ok(_) => return Err(S3Error::NotCreated("exists")),
            Err(error) if error.as_service_error().is_some_and(|e| e.is_not_found()) => {}
            Err(error) => {
                return Err(S3Error::NotCreated(
                    match error.raw_response().map(|r| r.status().as_u16()) {
                        Some(401 | 403) => "denied",
                        Some(_) => "refused",
                        None => "unreachable",
                    },
                ));
            }
        }
        // OVH selects the region through the endpoint (its documented S3api
        // create-bucket call has no AWS LocationConstraint payload).
        self.client
            .create_bucket()
            .bucket(bucket)
            .customize()
            .config_override(
                aws_sdk_s3::config::Builder::new()
                    .retry_config(aws_sdk_s3::config::retry::RetryConfig::disabled()),
            )
            .send()
            .await
            .map_err(|error| {
                // Only an explicit refusal proves the PUT created nothing; a
                // timeout or server error leaves the outcome uncertain.
                match error.as_service_error().and_then(|e| e.meta().code()) {
                    Some("InvalidAccessKeyId" | "SignatureDoesNotMatch") => {
                        S3Error::NotCreated("credentials")
                    }
                    Some("AccessDenied") => S3Error::NotCreated("denied"),
                    Some("BucketAlreadyExists" | "BucketAlreadyOwnedByYou") => {
                        S3Error::NotCreated("exists")
                    }
                    Some("InvalidBucketName") => S3Error::NotCreated("name"),
                    _ => s3_error(error),
                }
            })?;
        if !self.managed_bucket_is_empty(bucket).await? {
            return Err(S3Error::Request(
                "created bucket is not confirmed empty and writable".into(),
            ));
        }
        Ok(())
    }

    /// Whether `bucket` can become a VaultCenter destination: no object, no
    /// multipart upload, no version or delete marker, and tiering status
    /// `None` (writable). `Ok(false)` proves content or a closed bucket; a
    /// provider error is returned as is and proves nothing.
    pub async fn managed_bucket_is_empty(&self, bucket: &str) -> Result<bool, S3Error> {
        let mut configuration = self.config.clone();
        configuration.bucket = bucket.to_owned();
        let target = Self::connect(configuration).await;
        target.check_bucket().await?;
        let (objects, complete) = target.bucket_inventory().await?;
        if !objects.is_empty() {
            return Ok(false);
        }
        if !complete {
            return Err(S3Error::Request("incomplete bucket inventory".into()));
        }
        if target.incomplete_multipart_uploads().await? != 0
            || target.bucket_status().await?.status != "None"
        {
            return Ok(false);
        }
        let versions = target
            .client
            .list_object_versions()
            .bucket(bucket)
            .max_keys(1)
            .send()
            .await
            .map_err(s3_error)?;
        Ok(versions.versions().is_empty() && versions.delete_markers().is_empty())
    }

    pub fn config(&self) -> &S3Config {
        &self.config
    }

    /// Probe authentication and listing without writing any object.
    pub async fn probe_access(&self) -> Result<(), S3Error> {
        self.check_bucket().await?;
        self.client
            .list_objects_v2()
            .bucket(&self.config.bucket)
            .prefix(&self.config.prefix)
            .max_keys(1)
            .send()
            .await
            .map_err(s3_error)?;
        Ok(())
    }

    pub async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, S3Error> {
        let mut result = Vec::new();
        let mut token = None;
        loop {
            let page = self
                .client
                .list_objects_v2()
                .bucket(&self.config.bucket)
                .prefix(prefix)
                .set_continuation_token(token.clone())
                .send()
                .await
                .map_err(s3_error)?;
            result.extend(
                page.contents()
                    .iter()
                    .filter_map(|item| item.key().map(str::to_owned)),
            );
            if page.is_truncated() != Some(true) {
                break;
            }
            let next = page
                .next_continuation_token()
                .ok_or_else(|| S3Error::Request("missing S3 pagination token".into()))?
                .to_owned();
            if token.as_ref() == Some(&next) {
                return Err(S3Error::Request("repeated S3 pagination token".into()));
            }
            token = Some(next);
        }
        Ok(result)
    }

    pub async fn check_bucket(&self) -> Result<(), S3Error> {
        self.client
            .head_bucket()
            .bucket(&self.config.bucket)
            .send()
            .await
            .map_err(s3_error)?;
        Ok(())
    }

    /// Bucket-wide listing, without downloading objects. A capped inventory is
    /// explicitly incomplete and must never be used as an archive authorization.
    pub async fn bucket_inventory(&self) -> Result<(Vec<(String, u64)>, bool), S3Error> {
        let mut objects = Vec::new();
        let mut token = None;
        loop {
            let page = self
                .client
                .list_objects_v2()
                .bucket(&self.config.bucket)
                .set_continuation_token(token.clone())
                .send()
                .await
                .map_err(s3_error)?;
            for object in page.contents() {
                let key = object
                    .key()
                    .ok_or_else(|| S3Error::Request("missing object key".into()))?;
                let size = object
                    .size()
                    .and_then(|s| u64::try_from(s).ok())
                    .ok_or_else(|| S3Error::Request("invalid object size".into()))?;
                objects.push((key.to_owned(), size));
            }
            if page.is_truncated() != Some(true) {
                return Ok((objects, true));
            }
            if objects.len() >= 100_000 {
                return Ok((objects, false));
            }
            let next = page
                .next_continuation_token()
                .ok_or_else(|| S3Error::Request("missing pagination token".into()))?
                .to_owned();
            if token.as_ref() == Some(&next) {
                return Err(S3Error::Request("repeated pagination token".into()));
            }
            token = Some(next);
        }
    }

    /// Recovery can show verified manifests from earlier pages after a later
    /// listing failure. Partial results never authorize a bucket mutation.
    pub async fn recovery_inventory_objects(&self) -> Result<(Vec<(String, u64)>, bool), S3Error> {
        let mut objects = Vec::new();
        let mut token = None;
        let mut pages = 0;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = self
                .client
                .list_objects_v2()
                .bucket(&self.config.bucket)
                .prefix(self.config.object_key("cartridges/"))
                .set_continuation_token(token.clone())
                .send()
                .await;
            let page = match page {
                Ok(page) => page,
                Err(error) if pages == 0 => return Err(s3_error(error)),
                Err(_) => return Ok((objects, false)),
            };
            pages += 1;
            for object in page.contents() {
                let (Some(key), Some(size)) = (
                    object.key(),
                    object.size().and_then(|s| u64::try_from(s).ok()),
                ) else {
                    return Ok((objects, false));
                };
                objects.push((key.to_owned(), size));
            }
            if page.is_truncated() != Some(true) {
                return Ok((objects, true));
            }
            if objects.len() >= 100_000 || pages >= 1000 {
                return Ok((objects, false));
            }
            let Some(next) = page.next_continuation_token().filter(|s| !s.is_empty()) else {
                return Ok((objects, false));
            };
            if !seen.insert(next.to_owned()) {
                return Ok((objects, false));
            }
            token = Some(next.to_owned());
        }
    }

    pub async fn object_exists(&self, key: &str) -> Result<bool, S3Error> {
        match self
            .client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if error.as_service_error().is_some_and(|e| e.is_not_found()) => Ok(false),
            Err(error) => Err(s3_error(error)),
        }
    }

    /// Refuse historical versions and delete markers, including suspended buckets.
    pub async fn archive_versions_clear(&self) -> Result<(), S3Error> {
        let mut key = None;
        let mut version = None;
        for _ in 0..100 {
            let page = self
                .client
                .list_object_versions()
                .bucket(&self.config.bucket)
                .set_key_marker(key.clone())
                .set_version_id_marker(version.clone())
                .send()
                .await
                .map_err(s3_error)?;
            if !page.delete_markers().is_empty()
                || page.versions().iter().any(|v| {
                    v.is_latest() != Some(true) || !matches!(v.version_id(), None | Some("null"))
                })
            {
                return Err(S3Error::Request(
                    "historical or versioned objects require a separate archive workflow".into(),
                ));
            }
            if page.is_truncated() != Some(true) {
                return Ok(());
            }
            let next_key = page.next_key_marker().map(str::to_owned);
            let next_version = page.next_version_id_marker().map(str::to_owned);
            if next_key.is_none() || (key == next_key && version == next_version) {
                return Err(S3Error::Request("invalid version pagination".into()));
            }
            key = next_key;
            version = next_version;
        }
        Err(S3Error::Request("version inventory limit reached".into()))
    }

    /// Deletion must remove bytes, not merely create a versioned delete marker.
    pub async fn require_unversioned_deletion(&self) -> Result<(), S3Error> {
        let state = self
            .client
            .get_bucket_versioning()
            .bucket(&self.config.bucket)
            .send()
            .await
            .map_err(s3_error)?;
        if state.status().is_some() {
            return Err(S3Error::Request(
                "Deleting copies in a versioned bucket is not supported.".into(),
            ));
        }
        self.archive_versions_clear().await
    }

    pub async fn delete_object(&self, key: &str) -> Result<(), S3Error> {
        self.client
            .delete_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
            .map_err(s3_error)?;
        if self.object_exists(key).await? {
            return Err(S3Error::Request("S3 object deletion not confirmed.".into()));
        }
        Ok(())
    }

    pub async fn put_bytes(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
        checksum_sha256: &str,
    ) -> Result<StoredObject, S3Error> {
        let output = self
            .client
            .put_object()
            .bucket(&self.config.bucket)
            .key(key)
            .content_type(content_type)
            .metadata("cold-vtl-sha256", checksum_sha256)
            .body(self.object_body(ByteStream::from(bytes)))
            .send()
            .await
            .map_err(s3_error)?;

        Ok(StoredObject {
            key: key.to_owned(),
            etag: output.e_tag().map(ToOwned::to_owned),
            version_id: output.version_id().map(ToOwned::to_owned),
        })
    }

    /// Retryable body that reopens the same range, with memory independent of range size.
    pub async fn put_file_range(
        &self,
        key: &str,
        path: &Path,
        offset: u64,
        length: u64,
        checksum_sha256: &str,
    ) -> Result<StoredObject, S3Error> {
        let body = self.object_body(file_range(path, offset, length).await?);
        let output = self
            .client
            .put_object()
            .bucket(&self.config.bucket)
            .key(key)
            .content_type("application/octet-stream")
            .metadata("cold-vtl-sha256", checksum_sha256)
            .content_length(content_length(length)?)
            .body(body)
            .send()
            .await
            .map_err(s3_error)?;
        Ok(StoredObject {
            key: key.to_owned(),
            etag: output.e_tag().map(str::to_owned),
            version_id: output.version_id().map(str::to_owned),
        })
    }

    /// The caller must persist creation intent before this call. A lost response is
    /// ambiguous: S3 cannot return custom metadata for unfinished uploads, so it is
    /// unsafe to adopt an upload discovered only by matching its key.
    pub async fn create_multipart(
        &self,
        key: &str,
        checksum_sha256: &str,
    ) -> Result<String, S3Error> {
        let output = self
            .client
            .create_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .content_type("application/octet-stream")
            .metadata("cold-vtl-sha256", checksum_sha256)
            .customize()
            .config_override(
                aws_sdk_s3::config::Builder::new()
                    .retry_config(aws_sdk_s3::config::retry::RetryConfig::disabled()),
            )
            .send()
            .await
            .map_err(s3_error)?;
        output
            .upload_id()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| S3Error::Request("missing multipart upload ID".into()))
    }

    pub async fn upload_part_file(
        &self,
        key: &str,
        upload_id: &str,
        part_number: i32,
        path: &Path,
        offset: u64,
        length: u64,
    ) -> Result<MultipartPart, S3Error> {
        validate_part_number(part_number)?;
        if length == 0 {
            return Err(S3Error::Request("multipart part must not be empty".into()));
        }
        let body = self.object_body(file_range(path, offset, length).await?);
        let output = self
            .client
            .upload_part()
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload_id)
            .part_number(part_number)
            .content_length(content_length(length)?)
            .body(body)
            .send()
            .await
            .map_err(|error| {
                if error.as_service_error().and_then(|e| e.meta().code()) == Some("NoSuchUpload") {
                    S3Error::NoSuchUpload
                } else {
                    s3_error(error)
                }
            })?;
        let etag = output
            .e_tag()
            .filter(|etag| !etag.is_empty())
            .ok_or_else(|| S3Error::Request("missing multipart part ETag".into()))?;
        Ok(MultipartPart {
            checksum_sha256: None,
            part_number,
            etag: etag.to_owned(),
            size: length,
        })
    }

    pub async fn list_multipart_parts(
        &self,
        key: &str,
        upload_id: &str,
    ) -> Result<Vec<MultipartPart>, S3Error> {
        let mut result = Vec::new();
        let mut marker = None;
        loop {
            let page = self
                .client
                .list_parts()
                .bucket(&self.config.bucket)
                .key(key)
                .upload_id(upload_id)
                .set_part_number_marker(marker.clone())
                .max_parts(1000)
                .send()
                .await
                .map_err(|error| {
                    if error.as_service_error().and_then(|e| e.meta().code())
                        == Some("NoSuchUpload")
                    {
                        S3Error::NoSuchUpload
                    } else {
                        s3_error(error)
                    }
                })?;
            for part in page.parts() {
                let part_number = part
                    .part_number()
                    .ok_or_else(|| S3Error::Request("missing multipart part number".into()))?;
                validate_part_number(part_number)?;
                if result
                    .last()
                    .is_some_and(|last: &MultipartPart| last.part_number >= part_number)
                {
                    return Err(S3Error::Request(
                        "unordered or duplicate multipart part".into(),
                    ));
                }
                let etag = part
                    .e_tag()
                    .filter(|etag| !etag.is_empty())
                    .ok_or_else(|| S3Error::Request("missing multipart part ETag".into()))?;
                let size = part
                    .size()
                    .and_then(|size| u64::try_from(size).ok())
                    .filter(|size| *size > 0)
                    .ok_or_else(|| S3Error::Request("invalid multipart part size".into()))?;
                result.push(MultipartPart {
                    checksum_sha256: part.checksum_sha256().map(str::to_owned),
                    part_number,
                    etag: etag.to_owned(),
                    size,
                });
            }
            if page.is_truncated() != Some(true) {
                return Ok(result);
            }
            let next = page
                .next_part_number_marker()
                .and_then(|value| value.parse::<i32>().ok())
                .ok_or_else(|| S3Error::Request("invalid multipart pagination marker".into()))?;
            validate_part_number(next)?;
            if next
                <= marker
                    .as_deref()
                    .and_then(|value: &str| value.parse::<i32>().ok())
                    .unwrap_or(0)
                || next < result.last().map_or(0, |part| part.part_number)
            {
                return Err(S3Error::Request(
                    "non-progressing multipart pagination".into(),
                ));
            }
            marker = Some(next.to_string());
        }
    }

    /// Completion is deliberately not retried: the caller resolves an ambiguous
    /// response by verifying the resulting object's HEAD and streamed contents.
    pub async fn complete_multipart(
        &self,
        key: &str,
        upload_id: &str,
        parts: &[MultipartPart],
    ) -> Result<StoredObject, S3Error> {
        if parts.is_empty() || parts.len() > MAX_MULTIPART_PARTS as usize {
            return Err(S3Error::Request("invalid multipart part count".into()));
        }
        let mut completed = Vec::with_capacity(parts.len());
        for (index, part) in parts.iter().enumerate() {
            if part.part_number != index as i32 + 1 || part.etag.is_empty() || part.size == 0 {
                return Err(S3Error::Request(
                    "invalid multipart completion parts".into(),
                ));
            }
            completed.push(
                CompletedPart::builder()
                    .part_number(part.part_number)
                    .e_tag(&part.etag)
                    .build(),
            );
        }
        let output = self
            .client
            .complete_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(completed))
                    .build(),
            )
            .customize()
            .config_override(completion_override())
            .send()
            .await
            .map_err(|error| {
                if error.as_service_error().and_then(|e| e.meta().code()) == Some("NoSuchUpload") {
                    S3Error::NoSuchUpload
                } else {
                    s3_error(error)
                }
            })?;
        Ok(StoredObject {
            key: key.to_owned(),
            etag: output.e_tag().map(str::to_owned),
            version_id: output.version_id().map(str::to_owned),
        })
    }

    pub async fn abort_multipart(&self, key: &str, upload_id: &str) -> Result<(), S3Error> {
        self.client
            .abort_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
            .map_err(|error| {
                if error.as_service_error().and_then(|e| e.meta().code()) == Some("NoSuchUpload") {
                    S3Error::NoSuchUpload
                } else {
                    s3_error(error)
                }
            })?;
        Ok(())
    }

    pub async fn verify_head(
        &self,
        key: &str,
        expected_size: u64,
        expected_sha256: &str,
    ) -> Result<(), S3Error> {
        let output = self
            .client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
            .map_err(s3_error)?;
        let actual_size = output.content_length().unwrap_or_default();
        let actual_checksum = metadata_value(output.metadata(), "cold-vtl-sha256");

        if u64::try_from(actual_size).ok() != Some(expected_size)
            || actual_checksum != Some(expected_sha256)
        {
            return Err(S3Error::Verification {
                key: key.to_owned(),
                expected_size,
                actual_size,
                expected_sha256: expected_sha256.to_owned(),
                actual_sha256: actual_checksum.map(ToOwned::to_owned),
            });
        }

        Ok(())
    }

    pub async fn get_stream(&self, key: &str) -> Result<ByteStream, S3Error> {
        let output = self
            .client
            .get_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
            .map_err(s3_error)?;
        Ok(self.object_body(output.body))
    }

    /// Only small manifests may be collected. Segment readers must use get_stream.
    pub async fn get_bytes(&self, key: &str) -> Result<Vec<u8>, S3Error> {
        let mut body = self.get_stream(key).await?;
        if body.size_hint().0 > MAX_MANIFEST_BYTES as u64 {
            return Err(S3Error::ManifestTooLarge);
        }
        let mut result = Vec::new();
        while let Some(bytes) = body.try_next().await.map_err(s3_error)? {
            if bytes.len() > MAX_MANIFEST_BYTES - result.len() {
                return Err(S3Error::ManifestTooLarge);
            }
            result.extend_from_slice(&bytes);
        }
        Ok(result)
    }

    pub async fn bucket_status(&self) -> Result<BucketStatus, S3Error> {
        let output = self
            .client
            .get_bucket_tagging()
            .bucket(&self.config.bucket)
            .send()
            .await;

        let tags = match output {
            Ok(output) => output
                .tag_set()
                .iter()
                .map(|tag| (tag.key().to_owned(), tag.value().to_owned()))
                .collect::<HashMap<_, _>>(),
            Err(error)
                if error
                    .as_service_error()
                    .and_then(|service| service.meta().code())
                    == Some("NoSuchTagSet") =>
            {
                HashMap::new()
            }
            Err(error) => return Err(s3_error(error)),
        };

        Ok(BucketStatus {
            status: tags
                .get("ovh:intelligent_tiering_status")
                .cloned()
                .unwrap_or_else(|| "None".to_owned()),
            archive_lock_until: tags
                .get("ovh:intelligent_tiering_archive_lock_until")
                .cloned(),
        })
    }

    pub async fn incomplete_multipart_uploads(&self) -> Result<usize, S3Error> {
        let mut count = 0;
        let (mut key, mut upload) = (None, None);
        for _ in 0..100 {
            let page = self
                .client
                .list_multipart_uploads()
                .bucket(&self.config.bucket)
                .set_key_marker(key.clone())
                .set_upload_id_marker(upload.clone())
                .send()
                .await
                .map_err(s3_error)?;
            count += page.uploads().len();
            if page.is_truncated() != Some(true) {
                return Ok(count);
            }
            let next_key = page.next_key_marker().map(str::to_owned);
            let next_upload = page.next_upload_id_marker().map(str::to_owned);
            if next_key.is_none() || (next_key == key && next_upload == upload) {
                return Err(S3Error::Request("invalid multipart pagination".into()));
            }
            key = next_key;
            upload = next_upload;
        }
        Err(S3Error::Request("multipart inventory limit reached".into()))
    }

    pub async fn request_ovh_archive(
        &self,
        configuration_id: &str,
        lock_days: Option<i32>,
    ) -> Result<(), S3Error> {
        let (days, tier) = match lock_days {
            Some(days) if days > 0 => (days, "OVH_ARCHIVE_LOCK"),
            Some(_) => return Err(S3Error::InvalidLockDays),
            None => (999, "OVH_ARCHIVE"),
        };
        self.put_ovh_tiering(configuration_id, days, tier).await
    }

    pub async fn request_ovh_restore(&self, configuration_id: &str) -> Result<(), S3Error> {
        self.put_ovh_tiering(configuration_id, 999, "OVH_RESTORE")
            .await
    }

    pub async fn check_ovh_configuration(&self, configuration_id: &str) -> Result<(), S3Error> {
        let value = self
            .client
            .get_bucket_intelligent_tiering_configuration()
            .bucket(&self.config.bucket)
            .id(configuration_id)
            .send()
            .await
            .map_err(s3_error)?;
        if value.intelligent_tiering_configuration().is_none() {
            return Err(S3Error::Request(
                "archive configuration not confirmed".into(),
            ));
        }
        Ok(())
    }

    async fn put_ovh_tiering(
        &self,
        configuration_id: &str,
        days: i32,
        tier: &str,
    ) -> Result<(), S3Error> {
        let tiering = Tiering::builder()
            .days(days)
            .access_tier(IntelligentTieringAccessTier::from(tier))
            .build()
            .map_err(s3_error)?;
        let configuration = IntelligentTieringConfiguration::builder()
            .id(configuration_id)
            .status(IntelligentTieringStatus::Enabled)
            .tierings(tiering)
            .build()
            .map_err(s3_error)?;
        self.client
            .put_bucket_intelligent_tiering_configuration()
            .bucket(&self.config.bucket)
            .id(configuration_id)
            .intelligent_tiering_configuration(configuration)
            .customize()
            .config_override(
                aws_sdk_s3::config::Builder::new()
                    .retry_config(aws_sdk_s3::config::retry::RetryConfig::disabled()),
            )
            .send()
            .await
            .map_err(s3_error)?;
        Ok(())
    }
}

fn content_length(length: u64) -> Result<i64, S3Error> {
    i64::try_from(length).map_err(|_| S3Error::Request("file range is too large".into()))
}

fn validate_part_number(number: i32) -> Result<(), S3Error> {
    if !(1..=MAX_MULTIPART_PARTS).contains(&number) {
        return Err(S3Error::Request(
            "multipart part number must be in 1..=10000".into(),
        ));
    }
    Ok(())
}

async fn file_range(path: &Path, offset: u64, length: u64) -> Result<ByteStream, S3Error> {
    content_length(length)?;
    offset
        .checked_add(length)
        .ok_or_else(|| S3Error::Request("file range overflow".into()))?;
    ByteStream::read_from()
        .path(path)
        .offset(offset)
        .length(Length::Exact(length))
        .buffer_size(FILE_BUFFER_BYTES)
        .build()
        .await
        .map_err(s3_error)
}

fn required(name: &'static str) -> Result<String, ConfigError> {
    env::var(name).map_err(|_| ConfigError::Missing(name))
}

fn infer_region(endpoint: &str) -> &'static str {
    if endpoint.contains("rbx-archive") {
        "rbx-archive"
    } else if endpoint.contains("eu-west-par") {
        "eu-west-par"
    } else {
        "us-east-1"
    }
}

fn metadata_value<'a>(
    metadata: Option<&'a HashMap<String, String>>,
    name: &str,
) -> Option<&'a str> {
    metadata?.get(name).map(String::as_str)
}

/// OVH answers CompleteMultipartUpload with 200 at once, then sends nothing for
/// 10 s and only whitespace heartbeats until the object is assembled. The SDK
/// drops a response silent for 5 s, which failed every resume of CVT001L8 the
/// same way, so completion tolerates a long silence and is never retried.
const COMPLETION_SILENCE: std::time::Duration = std::time::Duration::from_secs(300);

fn completion_override() -> aws_sdk_s3::config::Builder {
    aws_sdk_s3::config::Builder::new()
        .retry_config(aws_sdk_s3::config::retry::RetryConfig::disabled())
        .stalled_stream_protection(
            aws_sdk_s3::config::StalledStreamProtectionConfig::enabled()
                .grace_period(COMPLETION_SILENCE)
                .build(),
        )
}

/// Keeps the whole source chain: an SDK error alone only says "service error".
fn s3_error(error: impl std::error::Error) -> S3Error {
    S3Error::Request(aws_smithy_types::error::display::DisplayErrorContext(error).to_string())
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    Missing(&'static str),
}

#[derive(Debug, Error)]
pub enum S3Error {
    #[error("multipart upload no longer exists")]
    NoSuchUpload,
    #[error("manifest exceeds the 16 MiB download limit")]
    ManifestTooLarge,
    #[error("S3 request failed: {0}")]
    Request(String),
    /// The provider answered before any bucket was created: nothing to recover.
    #[error("bucket not created: {0}")]
    NotCreated(&'static str),
    #[error("archive lock duration must be greater than zero days")]
    InvalidLockDays,
    #[error(
        "verification failed for {key}: expected {expected_size} bytes/{expected_sha256}, got {actual_size} bytes/{actual_sha256:?}"
    )]
    Verification {
        key: String,
        expected_size: u64,
        actual_size: i64,
        expected_sha256: String,
        actual_sha256: Option<String>,
    },
}

impl S3Error {
    pub fn is_no_such_upload(&self) -> bool {
        matches!(self, Self::NoSuchUpload)
    }
}

#[cfg(test)]
mod transfer_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recovery_listing_keeps_objects_when_a_later_page_is_denied() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for page in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                while !request.windows(4).any(|v| v == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.contains("prefix=vtl%2Fcartridges%2F"));
                if page == 1 {
                    assert!(request.contains("continuation-token=next"));
                }
                let (status, body) = if page == 0 {
                    (
                        200,
                        "<ListBucketResult><IsTruncated>true</IsTruncated><NextContinuationToken>next</NextContinuationToken><Contents><Key>vtl/cartridges/TAPE01/manifests/1.json</Key><Size>123</Size></Contents></ListBucketResult>",
                    )
                } else {
                    (403, "<Error><Code>AccessDenied</Code></Error>")
                };
                write!(stream,"HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let store = S3Store::connect(S3Config {
            provider: Some(S3Provider::S3Compatible),
            endpoint,
            region: "test".into(),
            bucket: "history".into(),
            prefix: "vtl".into(),
            access_key_id: "key".into(),
            secret_access_key: "secret".into(),
        })
        .await;
        let (objects, complete) = store.recovery_inventory_objects().await.unwrap();
        assert!(!complete);
        assert_eq!(
            objects,
            vec![("vtl/cartridges/TAPE01/manifests/1.json".into(), 123)]
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn discovery_paginates_and_preserves_partial_results_on_denial_or_repeated_token() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        for ending in ["complete", "denied", "repeated"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let mut requests = Vec::new();
                for page in 0..2 {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let mut buffer = [0u8; 4096];
                    loop {
                        let count = stream.read(&mut buffer).unwrap();
                        assert!(count > 0);
                        bytes.extend_from_slice(&buffer[..count]);
                        if bytes.windows(4).any(|v| v == b"\r\n\r\n") {
                            break;
                        }
                    }
                    requests.push(
                        String::from_utf8(bytes)
                            .unwrap()
                            .lines()
                            .next()
                            .unwrap()
                            .to_owned(),
                    );
                    let (status, body) = if page == 0 {
                        (
                            200,
                            "<ListAllMyBucketsResult><Buckets><Bucket><Name>history-one</Name></Bucket></Buckets><ContinuationToken>next</ContinuationToken></ListAllMyBucketsResult>",
                        )
                    } else {
                        match ending {
                            "denied" => (
                                403,
                                "<Error><Code>AccessDenied</Code><Message>secret-provider-details</Message></Error>",
                            ),
                            "repeated" => (
                                200,
                                "<ListAllMyBucketsResult><Buckets><Bucket><Name>history-two</Name></Bucket></Buckets><ContinuationToken>next</ContinuationToken></ListAllMyBucketsResult>",
                            ),
                            _ => (
                                200,
                                "<ListAllMyBucketsResult><Buckets><Bucket><Name>history-two</Name></Bucket><Bucket><Name>history-one</Name></Bucket></Buckets></ListAllMyBucketsResult>",
                            ),
                        }
                    };
                    write!(stream,"HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                }
                requests
            });
            let store = S3Store::connect(S3Config {
                provider: Some(S3Provider::S3Compatible),
                endpoint,
                region: "test".into(),
                bucket: String::new(),
                prefix: "vtl".into(),
                access_key_id: "key".into(),
                secret_access_key: "secret".into(),
            })
            .await;
            let result = store.discover_buckets().await;
            assert_eq!(result.complete, ending == "complete");
            assert_eq!(
                result.buckets,
                if ending == "denied" {
                    vec!["history-one"]
                } else {
                    vec!["history-one", "history-two"]
                }
            );
            assert_eq!(result.error.is_some(), ending != "complete");
            assert!(
                !serde_json::to_string(&result)
                    .unwrap()
                    .contains("secret-provider-details")
            );
            let requests = server.join().unwrap();
            assert!(
                requests[1].contains("continuation-token=next"),
                "{}",
                requests[1]
            );
        }
    }

    #[tokio::test]
    async fn create_bucket_refuses_existing_or_ambiguous_and_never_retries_put() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            sync::{Arc, Mutex},
            time::{Duration, Instant},
        };
        for head_status in [200, 403, 404] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let captured = Arc::new(Mutex::new(Vec::new()));
            let target = captured.clone();
            let server = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(3);
                'connections: while Instant::now() < deadline {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 4096];
                    loop {
                        let Ok(n) = stream.read(&mut buffer) else {
                            continue 'connections;
                        };
                        if n == 0 {
                            break;
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                        if bytes.windows(4).any(|v| v == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let request = String::from_utf8(bytes).unwrap();
                    let head = request.starts_with("HEAD ");
                    target.lock().unwrap().push(request);
                    let status = if head { head_status } else { 503 };
                    let body = if head {
                        ""
                    } else {
                        "<Error><Code>ServiceUnavailable</Code></Error>"
                    };
                    write!(stream,"HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                }
            });
            let store = S3Store::connect(S3Config {
                provider: Some(cold_vtl_core::S3Provider::OvhColdArchiveV1),
                endpoint,
                region: "rbx-archive".into(),
                bucket: "source-bucket".into(),
                prefix: "vtl".into(),
                access_key_id: "test-key".into(),
                secret_access_key: "test-secret".into(),
            })
            .await;
            assert!(
                store
                    .create_empty_ovh_bucket("vc-next-bucket")
                    .await
                    .is_err()
            );
            server.join().unwrap();
            let requests = captured.lock().unwrap();
            assert_eq!(requests.len(), if head_status == 404 { 2 } else { 1 });
            assert!(requests[0].starts_with("HEAD /vc-next-bucket"));
            if head_status == 404 {
                assert!(requests[1].starts_with("PUT /vc-next-bucket"));
                assert!(!requests[1].contains("LocationConstraint"));
            }
        }
    }

    #[tokio::test]
    async fn tiering_put_serializes_recall_and_does_not_retry_a_service_error() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            sync::{Arc, Mutex},
            time::{Duration, Instant},
        };
        for (status, success) in [(200, true), (503, false)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let captured = Arc::new(Mutex::new(Vec::new()));
            let target = captured.clone();
            let thread = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(3);
                while Instant::now() < deadline {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    };
                    // macOS inherits the listener's nonblocking flag on accept.
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 4096];
                    loop {
                        let count = stream.read(&mut buffer).unwrap();
                        if count == 0 {
                            break;
                        }
                        bytes.extend_from_slice(&buffer[..count]);
                        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                            let length = head
                                .lines()
                                .find_map(|line| {
                                    line.strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if bytes.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    target
                        .lock()
                        .unwrap()
                        .push(String::from_utf8(bytes).unwrap());
                    let body = if success {
                        ""
                    } else {
                        "<Error><Code>ServiceUnavailable</Code><Message>Try again</Message></Error>"
                    };
                    write!(stream,"HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nContent-Type: application/xml\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                }
            });
            let store = S3Store::connect(S3Config {
                provider: None,
                endpoint,
                region: "us-east-1".into(),
                bucket: "test-bucket".into(),
                prefix: "vtl".into(),
                access_key_id: "test-key".into(),
                secret_access_key: "test-secret".into(),
            })
            .await;
            assert_eq!(store.request_ovh_restore("cold-vtl").await.is_ok(), success);
            thread.join().unwrap();
            let requests = captured.lock().unwrap();
            assert_eq!(requests.len(), 1, "tiering PUT must not be retried");
            assert!(requests[0].starts_with("PUT /test-bucket/"));
            assert!(requests[0].contains("id=cold-vtl"));
            assert!(requests[0].contains("<AccessTier>OVH_RESTORE</AccessTier>"));
            assert!(!requests[0].contains("OVH_ARCHIVE_LOCK"));
        }
    }

    #[test]
    fn object_keys_are_normalized() {
        let config = S3Config {
            provider: None,
            endpoint: "https://example.invalid".to_owned(),
            region: "test".to_owned(),
            bucket: "bucket".to_owned(),
            prefix: "cold-vtl".to_owned(),
            access_key_id: "key".to_owned(),
            secret_access_key: "secret".to_owned(),
        };

        assert_eq!(
            config.object_key("/cartridges/TAPE01"),
            "cold-vtl/cartridges/TAPE01"
        );
    }

    #[test]
    fn known_ovh_regions_are_inferred_from_the_endpoint() {
        assert_eq!(
            infer_region("https://s3.rbx-archive.io.cloud.ovh.net"),
            "rbx-archive"
        );
        assert_eq!(
            infer_region("https://s3.eu-west-par.io.cloud.ovh.net"),
            "eu-west-par"
        );
    }
}
