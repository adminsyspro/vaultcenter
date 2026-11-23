use serde::{Deserialize, Serialize};

pub const OVH_ARCHIVE_ENDPOINT: &str = "https://s3.rbx-archive.io.cloud.ovh.net";

/// Every bucket VaultCenter creates starts with this prefix, so an S3 policy
/// scoped to `arn:aws:s3:::vc-*` covers the whole bucket lifecycle.
pub const MANAGED_BUCKET_PREFIX: &str = "vc-";

/// DNS-safe name for a bucket VaultCenter is about to create.
pub fn valid_managed_bucket_name(name: &str) -> bool {
    (3..=63).contains(&name.len())
        && name.starts_with(MANAGED_BUCKET_PREFIX)
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
}

/// Readable name for the next bucket of a VTL: `vc-<vtl>-<day>`, e.g.
/// `vc-veeam-2026-10-13`, so buckets can rotate weekly or monthly. When that
/// name is already `taken` (known to the catalogue or listed at the provider),
/// the first free `-2`, `-3`, … variant is used. Bucket names are global at the
/// provider: a name held by another account is not listed and simply fails at
/// creation (an existing bucket is never adopted); it is then recorded, so the
/// next attempt, e.g. the next export, proposes the following variant.
pub fn suggested_managed_bucket_name(
    vtl_name: &str,
    fallback: &str,
    date: &str,
    taken: impl Fn(&str) -> bool,
) -> String {
    let slug = |value: &str| {
        let mut slug = String::new();
        for c in value.chars().map(|c| c.to_ascii_lowercase()) {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                slug.push(c);
            } else if !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
        }
        slug.truncate(30);
        slug.trim_end_matches('-').to_owned()
    };
    let name = Some(slug(vtl_name))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| slug(fallback));
    let base = format!("{MANAGED_BUCKET_PREFIX}{name}-{date}");
    if !taken(&base) {
        return base;
    }
    // Bounded: creation itself refuses an existing bucket if every variant is known.
    let mut candidate = base.clone();
    for n in 2..=999 {
        candidate = format!("{base}-{n}");
        if !taken(&candidate) {
            break;
        }
    }
    candidate
}

/// Provider identity and capabilities are independent of credentials and bucket.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum S3Provider {
    OvhColdArchiveV1,
    S3Compatible,
}

#[derive(Serialize)]
pub struct S3ProviderDefinition {
    pub id: S3Provider,
    pub label: &'static str,
    pub default_endpoint: &'static str,
    pub default_region: &'static str,
    pub managed_archive: bool,
    pub description: &'static str,
}

impl S3Provider {
    pub const ALL: [Self; 2] = [Self::OvhColdArchiveV1, Self::S3Compatible];

    pub fn infer(endpoint: &str) -> Self {
        if endpoint.trim_end_matches('/') == OVH_ARCHIVE_ENDPOINT {
            Self::OvhColdArchiveV1
        } else {
            Self::S3Compatible
        }
    }

    pub fn validate_endpoint(self, endpoint: &str) -> Result<(), &'static str> {
        // A generic selection must never bypass the OVH bucket access gates.
        if self != Self::infer(endpoint) {
            return Err(
                "The selected provider does not match this endpoint. OVH Cold Archive v1 requires its Roubaix endpoint.",
            );
        }
        Ok(())
    }

    pub fn definition(self) -> S3ProviderDefinition {
        match self {
            Self::OvhColdArchiveV1 => S3ProviderDefinition {
                id: self,
                label: "OVHcloud Cold Archive v1",
                default_endpoint: OVH_ARCHIVE_ENDPOINT,
                default_region: "rbx-archive",
                managed_archive: true,
                description: "S3 exports, archiving and recall of the entire bucket via OVHcloud Cold Archive v1 (Roubaix).",
            },
            Self::S3Compatible => S3ProviderDefinition {
                id: self,
                label: "S3 compatible",
                default_endpoint: "",
                default_region: "us-east-1",
                managed_archive: false,
                description: "Exports and restoration of objects accessible via S3. Glacier archiving and recall are not managed for this provider.",
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suggested_names_use_the_vtl_name_and_day_then_count_when_taken() {
        let none = |_: &str| false;
        assert_eq!(
            suggested_managed_bucket_name("Veeam", "00000000", "2026-10-13", none),
            "vc-veeam-2026-10-13"
        );
        assert_eq!(
            suggested_managed_bucket_name(" Sauvegarde Été #2 ", "x", "2026-10-17", none),
            "vc-sauvegarde-t-2-2026-10-17"
        );
        assert_eq!(
            suggested_managed_bucket_name("***", "59033096", "2026-10-17", none),
            "vc-59033096-2026-10-17"
        );
        let taken = ["vc-pbs-2026-10-17", "vc-pbs-2026-10-17-2"];
        assert_eq!(
            suggested_managed_bucket_name("PBS", "x", "2026-10-17", |n| taken.contains(&n)),
            "vc-pbs-2026-10-17-3"
        );
        // The first free number wins, even when a later one is already known.
        let gap = ["vc-pbs-2026-10-17", "vc-pbs-2026-10-17-3"];
        assert_eq!(
            suggested_managed_bucket_name("PBS", "x", "2026-10-17", |n| gap.contains(&n)),
            "vc-pbs-2026-10-17-2"
        );
        let long = suggested_managed_bucket_name(&"a".repeat(100), "x", "2026-10-17", |_| true);
        assert!(valid_managed_bucket_name(&long), "{long}");
    }

    #[test]
    fn managed_bucket_names_carry_the_prefix_and_stay_dns_safe() {
        assert!(valid_managed_bucket_name(
            "vc-abc12345-2026-09-123456789abc"
        ));
        for name in [
            "",
            "vc-",
            "vc",
            "vaultcenter-2026-09",
            "VC-bucket",
            "vc-UPPER",
            "vc-a/b",
            "vc-bucket-",
            "vc-bucket.name",
            "xvc-bucket",
        ] {
            assert!(!valid_managed_bucket_name(name), "{name}");
        }
        assert!(!valid_managed_bucket_name(&format!(
            "vc-{}",
            "a".repeat(61)
        )));
    }
    #[test]
    fn legacy_inference_and_explicit_selection_cannot_bypass_archive_gates() {
        assert_eq!(
            S3Provider::infer(OVH_ARCHIVE_ENDPOINT),
            S3Provider::OvhColdArchiveV1
        );
        assert_eq!(
            S3Provider::infer("https://s3.example.test"),
            S3Provider::S3Compatible
        );
        assert!(
            S3Provider::S3Compatible
                .validate_endpoint(OVH_ARCHIVE_ENDPOINT)
                .is_err()
        );
        assert!(
            S3Provider::OvhColdArchiveV1
                .validate_endpoint("https://s3.example.test")
                .is_err()
        );
        assert!(
            S3Provider::OvhColdArchiveV1
                .validate_endpoint(&format!("{OVH_ARCHIVE_ENDPOINT}/"))
                .is_ok()
        );
        assert!(!S3Provider::S3Compatible.definition().managed_archive);
    }
}
