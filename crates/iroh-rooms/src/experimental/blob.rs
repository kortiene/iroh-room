//! **Experimental (unstable API).** Blob import, ACL-gated serve, and verified
//! fetch — the runtime half of [`crate::files`].
//!
//! `EndpointId` is re-exported here too (issue #87), verbatim from the pinned
//! `iroh` release — it is the type `BlobAclView::is_active` names — so a
//! consumer working only with blobs need not reach into
//! [`crate::experimental::session`].

pub use iroh::EndpointId;
pub use iroh_rooms_net::{BlobAclView, BlobError, BlobImport, BlobStore, FetchOutcome};
