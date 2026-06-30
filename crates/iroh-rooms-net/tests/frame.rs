//! Frame-codec integration tests: loopback QUIC pairs verify that `read_frame` /
//! `write_frame` handle round-trips, oversized declared lengths, and truncated
//! streams without panicking (spec §7.1 / implementation step 3). Every test
//! uses `RelayMode::Disabled` over 127.0.0.1 — deterministic, no real network.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use iroh::endpoint::{presets, Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use iroh_rooms_net::frame::{read_frame, write_frame, FrameError, MAX_FRAME_BYTES};

const FRAME_TEST_ALPN: &[u8] = b"/test/iroh-rooms-net/frame/1";
const TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Minimal loopback pair
// ---------------------------------------------------------------------------

/// A live loopback QUIC pair with one bidi stream.
///
/// The server (endpoint A) opens the stream and immediately finishes its send
/// half, which sends a STREAM+FIN frame over the wire. That frame is what makes
/// the client's `accept_bi()` return — without any write the stream stays
/// invisible to the peer (Quinn does not notify the acceptor until the opener
/// actually uses the stream). After setup:
///
/// - `client_send` → `server_recv`  (the direction all frame tests exercise)
/// - server's original send half is already FIN'd; `client_recv` yields EOF
///
/// All 9 frame tests only use `client_send` / `server_recv`, so this covers
/// every case without polluting the stream with a probe byte.
#[allow(dead_code)]
struct Pair {
    /// Write here to send data to the server side.
    pub client_send: SendStream,
    /// Read here to receive data the client sent.
    pub server_recv: RecvStream,
    /// Keep connection handles alive; dropping either closes all streams.
    _client_conn: Connection,
    _server_conn: Connection,
    /// Keep BOTH endpoints alive so their UDP sockets stay open. Dropping an
    /// endpoint closes the socket; the Quinn driver can no longer flush the
    /// send queue, so any buffered data silently disappears.
    _client_ep: Endpoint,
    _server_ep: Endpoint,
}

/// Build two loopback endpoints and establish one bidi QUIC stream between them.
///
/// `seed_a` / `seed_b` are secret-key seeds; vary them per test to avoid
/// port conflicts when tests run in parallel.
async fn make_pair(seed_a: u8, seed_b: u8) -> Pair {
    // --- Server endpoint (a) ---
    let a_key = SecretKey::from_bytes(&[seed_a; 32]);
    let a_ep = Endpoint::builder(presets::Minimal)
        .secret_key(a_key)
        .relay_mode(RelayMode::Disabled)
        .alpns(vec![FRAME_TEST_ALPN.to_vec()])
        .bind()
        .await
        .expect("server bind");

    let port = a_ep
        .bound_sockets()
        .into_iter()
        .map(|s| s.port())
        .next()
        .expect("bound port");
    let a_id = a_ep.id();
    let a_addr =
        EndpointAddr::new(a_id).with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));

    // --- Client endpoint (b) ---
    let b_key = SecretKey::from_bytes(&[seed_b; 32]);
    let b_ep = Endpoint::builder(presets::Minimal)
        .secret_key(b_key)
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .expect("client bind");

    // --- QUIC handshake (concurrent to avoid accept/connect race) ---
    let (server_conn, client_conn) = tokio::time::timeout(TIMEOUT, async {
        tokio::join!(
            async {
                a_ep.accept()
                    .await
                    .expect("server Incoming")
                    .await
                    .expect("server handshake")
            },
            async {
                b_ep.connect(a_addr, FRAME_TEST_ALPN)
                    .await
                    .expect("client connect")
            },
        )
    })
    .await
    .expect("connect/accept timeout");

    // --- Stream setup ---
    //
    // The server opens the bidi stream and immediately finishes its send half.
    // `finish()` queues a STREAM+FIN frame which the QUIC driver flushes in the
    // background. Receiving that frame is what makes `client_conn.accept_bi()`
    // return — until some bytes (or FIN) are sent by the opener, the stream
    // does not exist from the acceptor's point of view (per Quinn spec).
    //
    // Result:
    //   server_recv  = server's recv half = reads what the client sends
    //   client_send  = client's send half = sends to the server
    //   client_recv  = yields EOF immediately (server's send already FIN'd)
    let (server_recv, client_send) = tokio::time::timeout(TIMEOUT, async {
        tokio::join!(
            async {
                let (mut send, recv) = server_conn.open_bi().await.expect("server open_bi");
                // Finish the server's send half: sends STREAM+FIN to the client,
                // which is what makes accept_bi() return on the client side.
                let _ = send.finish();
                recv
            },
            async {
                let (send, _recv) = client_conn.accept_bi().await.expect("client accept_bi");
                // _recv already has EOF (server sent FIN above); not used by any test.
                send
            },
        )
    })
    .await
    .expect("stream setup timeout");

    Pair {
        client_send,
        server_recv,
        _client_conn: client_conn,
        _server_conn: server_conn,
        _client_ep: b_ep,
        _server_ep: a_ep,
    }
}

// ---------------------------------------------------------------------------
// Round-trip correctness
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frame_round_trip_small_body() {
    let p = make_pair(0x10, 0x11).await;
    let mut send = p.client_send;
    let mut recv = p.server_recv;

    let body: &[u8] = b"hello, loopback QUIC";
    write_frame(&mut send, body).await.expect("write");
    let got = tokio::time::timeout(TIMEOUT, read_frame(&mut recv))
        .await
        .expect("read timeout")
        .expect("no io error")
        .expect("some frame");
    assert_eq!(got, body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frame_round_trip_empty_body() {
    let p = make_pair(0x20, 0x21).await;
    let mut send = p.client_send;
    let mut recv = p.server_recv;

    write_frame(&mut send, b"").await.expect("write empty");
    let got = tokio::time::timeout(TIMEOUT, read_frame(&mut recv))
        .await
        .expect("read timeout")
        .expect("no io error")
        .expect("some frame");
    assert!(got.is_empty(), "empty body must round-trip as empty Vec");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frame_multiple_sequential_frames_ordered() {
    let p = make_pair(0x30, 0x31).await;
    let mut send = p.client_send;
    let mut recv = p.server_recv;

    // Write all frames first, then read — ordering must be preserved.
    let bodies: &[&[u8]] = &[b"alpha", b"", b"gamma", b"\x00\xFF\xAB"];
    for body in bodies {
        write_frame(&mut send, body).await.expect("write");
    }
    for expected in bodies {
        let got = tokio::time::timeout(TIMEOUT, read_frame(&mut recv))
            .await
            .expect("read timeout")
            .expect("no io error")
            .expect("some frame");
        assert_eq!(got.as_slice(), *expected);
    }
}

// ---------------------------------------------------------------------------
// Clean-EOF behaviour
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frame_clean_eof_at_frame_boundary_returns_none() {
    let p = make_pair(0x40, 0x41).await;
    let mut send = p.client_send;
    let mut recv = p.server_recv;

    // Close without writing anything — clean EOF exactly at the prefix boundary.
    let _ = send.finish();

    let result = tokio::time::timeout(TIMEOUT, read_frame(&mut recv))
        .await
        .expect("read timeout")
        .expect("no io error");
    assert!(result.is_none(), "clean EOF must yield Ok(None)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frame_eof_after_one_frame_yields_none_on_second_read() {
    let p = make_pair(0x42, 0x43).await;
    let mut send = p.client_send;
    let mut recv = p.server_recv;

    write_frame(&mut send, b"payload").await.expect("write");
    let _ = send.finish();

    let frame = tokio::time::timeout(TIMEOUT, read_frame(&mut recv))
        .await
        .expect("timeout")
        .expect("no error")
        .expect("frame");
    assert_eq!(frame, b"payload");

    let eof = tokio::time::timeout(TIMEOUT, read_frame(&mut recv))
        .await
        .expect("timeout")
        .expect("no io error");
    assert!(eof.is_none(), "second read after stream close must be None");
}

// ---------------------------------------------------------------------------
// Oversized guards
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frame_write_oversized_body_rejected_before_io() {
    // write_frame checks the length guard before touching the stream, so this
    // test passes even though no bytes reach the server.
    let p = make_pair(0x50, 0x51).await;
    let mut send = p.client_send;

    let big = vec![0u8; MAX_FRAME_BYTES as usize + 1];
    let err = write_frame(&mut send, &big)
        .await
        .expect_err("oversized body must fail");
    assert!(
        matches!(err, FrameError::Oversized { .. }),
        "expected FrameError::Oversized, got: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frame_read_oversized_declared_length_rejected() {
    // Bypass write_frame's guard and write a raw prefix claiming MAX_FRAME_BYTES + 1.
    let p = make_pair(0x60, 0x61).await;
    let mut send = p.client_send;
    let mut recv = p.server_recv;

    let bad_prefix = (MAX_FRAME_BYTES + 1).to_be_bytes();
    send.write_all(&bad_prefix).await.expect("bad prefix");
    send.write_all(b"filler").await.expect("filler");

    let err = tokio::time::timeout(TIMEOUT, read_frame(&mut recv))
        .await
        .expect("read timeout")
        .expect_err("oversized declared length must fail");
    assert!(
        matches!(err, FrameError::Oversized { declared } if declared == MAX_FRAME_BYTES + 1),
        "expected Oversized {{ declared: {} }}, got: {err}",
        MAX_FRAME_BYTES + 1,
    );
}

// ---------------------------------------------------------------------------
// Truncation: clean errors, no panics
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frame_truncated_prefix_returns_truncated_error() {
    let p = make_pair(0x70, 0x71).await;
    let mut send = p.client_send;
    let mut recv = p.server_recv;

    // Write only 2 of the required 4 prefix bytes, then close.
    send.write_all(&[0u8; 2]).await.expect("partial prefix");
    let _ = send.finish();

    let err = tokio::time::timeout(TIMEOUT, read_frame(&mut recv))
        .await
        .expect("read timeout")
        .expect_err("truncated prefix must fail");
    assert!(
        matches!(err, FrameError::Truncated),
        "expected FrameError::Truncated, got: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frame_truncated_body_returns_truncated_error() {
    let p = make_pair(0x80, 0x81).await;
    let mut send = p.client_send;
    let mut recv = p.server_recv;

    // Claim 50 bytes in the prefix, write only 10, then close.
    send.write_all(&50u32.to_be_bytes()).await.expect("prefix");
    send.write_all(&[0xAB; 10]).await.expect("partial body");
    let _ = send.finish();

    let err = tokio::time::timeout(TIMEOUT, read_frame(&mut recv))
        .await
        .expect("read timeout")
        .expect_err("truncated body must fail");
    assert!(
        matches!(err, FrameError::Truncated),
        "expected FrameError::Truncated, got: {err}"
    );
}
