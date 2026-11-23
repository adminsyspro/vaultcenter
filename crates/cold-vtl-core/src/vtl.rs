use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use uuid::Uuid;

macro_rules! identifier {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl FromStr for $name {
            type Err = uuid::Error;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

identifier!(VtlId);
identifier!(DestinationId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_validate_and_canonicalize_uuids() {
        assert!("not-a-library".parse::<VtlId>().is_err());
        assert!("../destination".parse::<DestinationId>().is_err());
        let id = "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA"
            .parse::<VtlId>()
            .unwrap();
        assert_eq!(id.to_string(), "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
        assert_eq!(id.to_string().parse::<VtlId>().unwrap(), id);
        assert_ne!(VtlId::new(), VtlId::new());
    }
}
