//! Client-side helpers shared by this crate's integration tests: framing one
//! message, reading one frame, and the handshake preamble. Kept as a `common`
//! module rather than duplicated per test file so a wire-contract change
//! shows up as one diff here, not a drift between suites.
#![allow(clippy::unwrap_used)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use hop_protocol::framing::{FRAME_PREFIX_LEN, decode_payload, encode_frame, payload_len};
use hop_protocol::{API_VERSION, ClientMsg, DaemonMsg};

/// Sends `msg` as one length-prefixed frame, through the same
/// [`hop_protocol::framing`] functions the daemon itself uses to decode —
/// so a test failure here means the wire contract broke, not that this
/// helper drifted from it.
pub fn send(stream: &mut UnixStream, msg: &ClientMsg) {
    let frame = encode_frame(msg).expect("test message must encode");
    stream
        .write_all(&frame)
        .expect("write to hopd must succeed");
}

/// Reads exactly one length-prefixed frame and decodes it as a [`DaemonMsg`].
pub fn recv(stream: &mut UnixStream) -> DaemonMsg {
    let mut prefix = [0u8; FRAME_PREFIX_LEN];
    stream
        .read_exact(&mut prefix)
        .expect("hopd must reply with a frame");
    let len = payload_len(prefix).expect("hopd's own prefix must be in-cap");
    let mut payload = vec![0u8; len];
    stream
        .read_exact(&mut payload)
        .expect("hopd's declared payload length must be honored");
    decode_payload(&payload).expect("hopd's reply must decode as a DaemonMsg")
}

pub fn hello(stream: &mut UnixStream) {
    send(
        stream,
        &ClientMsg::Hello {
            api_version: API_VERSION,
        },
    );
    let reply = recv(stream);
    assert_eq!(
        reply,
        DaemonMsg::HelloAck {
            api_version: API_VERSION
        }
    );
}
