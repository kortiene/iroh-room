//! **Experimental (unstable API).** Live-pipe forwarding: authenticated
//! TCP-over-QUIC splicing to an explicitly authorized room peer — the runtime
//! half of [`crate::pipes`].
//!
//! `EndpointId` is re-exported here too (issue #87), verbatim from the pinned
//! `iroh` release — it is the type `PipeSessionInfo.device` and the
//! `PipeAuditSink` callbacks name — so a consumer working only with pipes need
//! not reach into [`crate::experimental::session`].

pub use iroh::EndpointId;
pub use iroh_rooms_net::pipe::is_loopback_target;
pub use iroh_rooms_net::{
    new_pipe_id, PipeAuditSink, PipeDenyCause, PipeError, PipeForwarder, PipeOutcome, PipeRegistry,
    PipeSessionInfo, TracingPipeAudit, PIPE_ALPN,
};
