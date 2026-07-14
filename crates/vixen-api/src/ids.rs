//! Stable, dependency-free identifiers used across the browser command seam.

use std::fmt;
use std::num::NonZeroU64;

/// Returned when an adapter attempts to construct an identifier from zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidId {
    kind: &'static str,
}

impl InvalidId {
    pub const fn kind(self) -> &'static str {
        self.kind
    }
}

impl fmt::Display for InvalidId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} must be non-zero", self.kind)
    }
}

impl std::error::Error for InvalidId {}

macro_rules! define_id {
    ($name:ident) => {
        #[doc = concat!("Stable non-zero `", stringify!($name), "` value allocated by the browser core.")]
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(NonZeroU64);

        impl $name {
            /// Construct an id at an adapter/persistence boundary.
            pub const fn new(raw: u64) -> Option<Self> {
                match NonZeroU64::new(raw) {
                    Some(raw) => Some(Self(raw)),
                    None => None,
                }
            }

            /// Return the protocol/persistence representation.
            pub const fn get(self) -> u64 {
                self.0.get()
            }
        }

        impl TryFrom<u64> for $name {
            type Error = InvalidId;

            fn try_from(raw: u64) -> Result<Self, Self::Error> {
                Self::new(raw).ok_or(InvalidId {
                    kind: stringify!($name),
                })
            }
        }

        impl From<$name> for u64 {
            fn from(id: $name) -> Self {
                id.get()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.get().fmt(formatter)
            }
        }
    };
}

define_id!(ProfileId);
define_id!(BrowserId);
define_id!(BrowsingContextId);
define_id!(FrameId);
define_id!(NavigationId);
define_id!(DocumentId);
define_id!(RequestId);
define_id!(RuntimeContextId);
define_id!(DownloadId);
define_id!(RenderNodeId);
define_id!(RenderResourceId);
define_id!(RenderFragmentId);
define_id!(RenderCommitId);
define_id!(RenderHandleId);
define_id!(RenderQueryId);
define_id!(RenderScrollNodeId);
define_id!(RenderScrollCommandId);
define_id!(SemanticNodeId);
define_id!(SemanticActionRequestId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_reject_zero_and_round_trip() {
        assert_eq!(BrowsingContextId::new(0), None);
        let id = BrowsingContextId::try_from(42).unwrap();
        assert_eq!(id.get(), 42);
        assert_eq!(id.to_string(), "42");
        assert_eq!(u64::from(id), 42);
    }

    #[test]
    fn invalid_id_names_the_boundary_type() {
        let error = RequestId::try_from(0).unwrap_err();
        assert_eq!(error.kind(), "RequestId");
        assert_eq!(error.to_string(), "RequestId must be non-zero");
    }

    #[test]
    fn optional_ids_keep_the_nonzero_niche() {
        assert_eq!(
            std::mem::size_of::<Option<DocumentId>>(),
            std::mem::size_of::<u64>()
        );
    }
}
