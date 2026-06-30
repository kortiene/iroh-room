//! Bidirectional TCP↔QUIC byte splicing (spec §4.2 / D2 / R1).
//!
//! A pipe bidi stream and a loopback [`TcpStream`] are spliced by two independent
//! copy loops — QUIC→TCP and TCP→QUIC — joined until both ends close. Half-close is
//! propagated cleanly (R1): when one side reaches EOF its copy loop shuts the other
//! side's write half (`TcpStream::shutdown` / `SendStream::finish`), so a peer that
//! finishes sending still drains the reverse direction. The QUIC side uses iroh's
//! inherent stream API (the same `read_exact`/`write_all`/`finish` the event-plane
//! framing uses); the TCP side uses tokio's `AsyncRead`/`AsyncWrite`.

use iroh::endpoint::{RecvStream, SendStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Copy buffer size for each direction (16 KiB; a pragmatic default for loopback).
const SPLICE_BUF: usize = 16 * 1024;

/// Splice a QUIC bidi stream (`q_send`/`q_recv`) to a loopback `tcp` stream until
/// both directions close. Returns when both copy loops have finished.
pub async fn splice(mut q_send: SendStream, mut q_recv: RecvStream, tcp: TcpStream) {
    let (mut tcp_read, mut tcp_write) = tcp.into_split();

    // QUIC → TCP: drain the stream into the local socket; on QUIC EOF, FIN the TCP
    // write half so the target service sees the client close.
    let q_to_tcp = async move {
        let mut buf = vec![0u8; SPLICE_BUF];
        while let Ok(Some(n)) = q_recv.read(&mut buf).await {
            if tcp_write.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
        let _ = tcp_write.shutdown().await;
    };

    // TCP → QUIC: drain the local socket into the stream; on TCP EOF, `finish()` the
    // QUIC send so the peer sees the close.
    let tcp_to_q = async move {
        let mut buf = vec![0u8; SPLICE_BUF];
        while let Ok(n) = tcp_read.read(&mut buf).await {
            if n == 0 || q_send.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
        let _ = q_send.finish();
    };

    tokio::join!(q_to_tcp, tcp_to_q);
}
