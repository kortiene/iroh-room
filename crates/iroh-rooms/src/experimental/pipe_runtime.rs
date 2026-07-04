//! **Experimental (unstable API).** Live-pipe forwarding: authenticated
//! TCP-over-QUIC splicing to an explicitly authorized room peer — the runtime
//! half of [`crate::pipes`].

pub use iroh_rooms_net::pipe::is_loopback_target;
pub use iroh_rooms_net::{
    new_pipe_id, PipeAuditSink, PipeDenyCause, PipeError, PipeForwarder, PipeOutcome, PipeRegistry,
    PipeSessionInfo, TracingPipeAudit, PIPE_ALPN,
};
