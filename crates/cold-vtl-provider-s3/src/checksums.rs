//! Explicit server-validated checksums. No metadata/ETag fallback.
use super::*;
use aws_sdk_s3::types::{ChecksumAlgorithm, ChecksumMode, ChecksumType};
use cold_vtl_core::NativeChecksum;

fn require_digest(actual: Option<&str>, expected: &str) -> Result<(), S3Error> {
    if actual != Some(expected) {
        return Err(S3Error::Request(
            "S3 did not confirm the expected SHA-256 checksum".into(),
        ));
    }
    Ok(())
}
impl S3Store {
    pub async fn put_checked_bytes(
        &self,
        key: &str,
        bytes: Vec<u8>,
        checksum: &str,
    ) -> Result<(), S3Error> {
        let output = self
            .client
            .put_object()
            .bucket(&self.config.bucket)
            .key(key)
            .checksum_sha256(checksum)
            .body(self.object_body(ByteStream::from(bytes)))
            .send()
            .await
            .map_err(s3_error)?;
        require_digest(output.checksum_sha256(), checksum)
    }

    pub async fn create_checked_multipart(&self, key: &str) -> Result<String, S3Error> {
        let output = self
            .client
            .create_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .checksum_algorithm(ChecksumAlgorithm::Sha256)
            .checksum_type(ChecksumType::Composite)
            .customize()
            .config_override(
                aws_sdk_s3::config::Builder::new()
                    .retry_config(aws_sdk_s3::config::retry::RetryConfig::disabled()),
            )
            .send()
            .await
            .map_err(s3_error)?;
        // Persist the returned ID even if a provider omits echoed algorithm headers;
        // UploadPart and HEAD still must prove the exact checksums.
        output
            .upload_id()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| S3Error::Request("missing multipart upload ID".into()))
    }

    pub async fn upload_checked_part(
        &self,
        key: &str,
        upload: &str,
        number: i32,
        bytes: Vec<u8>,
        checksum: &str,
    ) -> Result<MultipartPart, S3Error> {
        validate_part_number(number)?;
        let size = bytes.len() as u64;
        if size == 0 {
            return Err(S3Error::Request("empty multipart part".into()));
        }
        let output = self
            .client
            .upload_part()
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload)
            .part_number(number)
            .checksum_sha256(checksum)
            .body(self.object_body(ByteStream::from(bytes)))
            .send()
            .await
            .map_err(s3_error)?;
        require_digest(output.checksum_sha256(), checksum)?;
        let etag = output
            .e_tag()
            .filter(|e| !e.is_empty())
            .ok_or_else(|| S3Error::Request("missing part ETag".into()))?;
        Ok(MultipartPart {
            part_number: number,
            etag: etag.into(),
            size,
            checksum_sha256: Some(checksum.into()),
        })
    }

    pub async fn complete_checked_multipart(
        &self,
        key: &str,
        upload: &str,
        parts: &[MultipartPart],
        checksum: &str,
    ) -> Result<(), S3Error> {
        if parts.is_empty() || parts.len() > MAX_MULTIPART_PARTS as usize {
            return Err(S3Error::Request("invalid part count".into()));
        }
        let mut completed = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            let digest = part
                .checksum_sha256
                .as_deref()
                .ok_or_else(|| S3Error::Request("missing confirmed part checksum".into()))?;
            if part.part_number != i as i32 + 1 || part.size == 0 || part.etag.is_empty() {
                return Err(S3Error::Request("invalid confirmed part".into()));
            }
            completed.push(
                CompletedPart::builder()
                    .part_number(part.part_number)
                    .e_tag(&part.etag)
                    .checksum_sha256(digest)
                    .build(),
            );
        }
        let output = self
            .client
            .complete_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload)
            .checksum_type(ChecksumType::Composite)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(completed))
                    .build(),
            )
            .customize()
            .config_override(completion_override())
            .send()
            .await
            .map_err(s3_error)?;
        require_digest(output.checksum_sha256(), checksum)
    }

    pub async fn verify_native_head(
        &self,
        key: &str,
        size: u64,
        expected: &NativeChecksum,
    ) -> Result<(), S3Error> {
        if !expected.validate() {
            return Err(S3Error::Request("invalid native checksum".into()));
        }
        let output = self
            .client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key)
            .checksum_mode(ChecksumMode::Enabled)
            .send()
            .await
            .map_err(s3_error)?;
        require_digest(output.checksum_sha256(), &expected.value)?;
        if output.content_length().and_then(|n| u64::try_from(n).ok()) != Some(size)
            || output.checksum_type().map(|t| t.as_str()) != Some(expected.kind.as_str())
        {
            return Err(S3Error::Request(
                "S3 object size or checksum type differs".into(),
            ));
        }
        Ok(())
    }
}
