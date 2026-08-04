//! Length-prefixed framing over bytes.
//!
//! A frame on the wire is `[4-byte big-endian payload length][JSON payload]`.
//! This module is the one place that shape is written down, so the tokio
//! daemon and the blocking-std CLI decode the same bytes the same way instead
//! of each carrying its own copy of the prefix arithmetic.
//!
//! # This module is deliberately IO-free
//!
//! Every function here is a pure transform over bytes already in memory: no
//! socket, no file, no `async`. Reading the prefix and the payload off a real
//! transport is the transport's job, not this module's — a tokio daemon reads
//! with `AsyncReadExt`, a blocking-std CLI reads with `std::io::Read`, and both
//! call the same [`payload_len`] and [`decode_payload`] once the bytes are in
//! hand. Keeping the codec IO-free is what lets both sides share it: an
//! `async` dependency here would either force it on the blocking CLI or fork
//! the codec in two.
//!
//! # `payload_len` is the pre-allocation gate
//!
//! A length prefix is the one part of a frame a transport must trust before it
//! has read anything else, because the prefix is what says how much more to
//! read. [`payload_len`] decodes it and refuses, with
//! [`FrameError::TooLarge`], any value over
//! [`MAX_FRAME_BYTES`](crate::limits::MAX_FRAME_BYTES) — before a caller
//! allocates a buffer sized by that value. A transport that reads the prefix,
//! calls `payload_len`, and only then allocates cannot be made to allocate an
//! attacker-chosen amount; a transport that allocates first and checks after
//! already has.

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::limits::MAX_FRAME_BYTES;

/// Byte length of a frame's length prefix.
///
/// The prefix is a `u32` big-endian count of the payload's bytes, so this is
/// always 4 — not a knob, just named so `payload_len`'s signature and the
/// split a transport does on a frame's first bytes both read as the same
/// number rather than a bare literal repeated in two places.
pub const FRAME_PREFIX_LEN: usize = 4;

/// Something that went wrong turning a message into a frame, or a frame back
/// into a message.
#[derive(Debug, Error)]
pub enum FrameError {
    /// A length — a decoded prefix, or a payload about to be encoded — is over
    /// [`MAX_FRAME_BYTES`](crate::limits::MAX_FRAME_BYTES).
    ///
    /// Carries the offending length rather than the message itself: the whole
    /// point of catching this before [`decode_payload`] runs is that the
    /// payload the length describes is never read into memory to be reported.
    #[error("frame of {len} bytes is over the {MAX_FRAME_BYTES}-byte cap")]
    TooLarge {
        /// The length that broke the cap, in bytes.
        len: usize,
    },

    /// [`encode_frame`] could not serialize the message to JSON.
    ///
    /// Kept distinct from [`FrameError::Decode`] even though both wrap a
    /// [`serde_json::Error`]: this side is this process failing to write a
    /// message it built itself — a bug here, never a hostile peer — and
    /// collapsing it with a peer's malformed bytes would make a log line
    /// misreport which side is at fault.
    #[error("failed to encode frame payload: {0}")]
    Encode(#[from] serde_json::Error),

    /// [`decode_payload`] could not parse a frame's payload as JSON.
    ///
    /// Not `#[from]`, unlike [`FrameError::Encode`]: both variants wrap the
    /// same error type, and if both derived `From<serde_json::Error>` the `?`
    /// operator in [`decode_payload`] would have no way to pick one, so this
    /// one is constructed explicitly instead. The payload behind this error
    /// came off the wire from a peer this crate does not trust — see this
    /// crate's [`limits`](crate::limits) docs — so a parse failure here is
    /// exactly the kind of thing a hostile or broken peer is expected to
    /// trigger, not a bug in this process.
    #[error("failed to decode frame payload: {0}")]
    Decode(serde_json::Error),
}

/// Refuses `len` if it is over [`MAX_FRAME_BYTES`](crate::limits::MAX_FRAME_BYTES).
///
/// The one check both [`encode_frame`] and [`payload_len`] funnel through, so
/// the cap is applied identically to a payload this process is about to write
/// and to a prefix a peer sent claiming how much it is about to send. Kept
/// private rather than exposed as public API: the brief this module
/// implements asks for exactly three public functions, and a unit test can
/// still reach this one through `super::*` without it being part of the
/// crate's surface.
fn ensure_within_cap(len: usize) -> Result<(), FrameError> {
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge { len });
    }
    Ok(())
}

/// Serializes `msg` to JSON and returns a complete frame: the
/// [`FRAME_PREFIX_LEN`]-byte big-endian payload length, followed by the
/// payload.
///
/// Refuses a payload over [`MAX_FRAME_BYTES`](crate::limits::MAX_FRAME_BYTES)
/// after serializing but before the prefix or the returned `Vec` is built, so
/// an over-cap message never gets as far as looking like a frame this crate
/// would hand to a transport.
pub fn encode_frame<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError> {
    let payload = serde_json::to_vec(msg)?;
    ensure_within_cap(payload.len())?;

    // `payload.len()` has just been checked against `MAX_FRAME_BYTES`, which
    // is far under `u32::MAX`, so this cast never truncates.
    let mut frame = Vec::with_capacity(FRAME_PREFIX_LEN + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decodes a frame's length prefix, refusing any value over
/// [`MAX_FRAME_BYTES`](crate::limits::MAX_FRAME_BYTES).
///
/// This is the pre-allocation gate this module's docs describe: a transport
/// reads exactly [`FRAME_PREFIX_LEN`] bytes, calls this, and only allocates a
/// payload buffer once it returns `Ok`. A caller that allocates before calling
/// this — or that skips the call — has undone the guarantee this function
/// exists to give.
pub fn payload_len(prefix: [u8; FRAME_PREFIX_LEN]) -> Result<usize, FrameError> {
    let len = u32::from_be_bytes(prefix) as usize;
    ensure_within_cap(len)?;
    Ok(len)
}

/// Parses a frame's payload bytes as `T`.
///
/// A thin `serde_json::from_slice` wrapper: the size of `payload` is a
/// transport's concern, already settled by [`payload_len`] before these bytes
/// were read. What this function adds is the field-level bounds in
/// [`limits`](crate::limits), which fire during this parse the same way they
/// do for any other `serde_json` call — an in-cap frame can still carry a
/// `query` whose `text` breaks `MAX_QUERY_TEXT`, and that is refused here, not
/// by the framing layer.
pub fn decode_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, FrameError> {
    serde_json::from_slice(payload).map_err(FrameError::Decode)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::limits::MAX_FRAME_BYTES;
    use crate::wire::ClientMsg;

    #[test]
    fn a_frame_round_trips_through_encode_and_decode() {
        let msg = ClientMsg::Hello { api_version: 1 };
        let frame = encode_frame(&msg).unwrap();

        let (prefix, payload) = frame.split_at(FRAME_PREFIX_LEN);
        let prefix: [u8; FRAME_PREFIX_LEN] = prefix.try_into().unwrap();
        let len = payload_len(prefix).unwrap();
        assert_eq!(len, payload.len());

        let decoded: ClientMsg = decode_payload(payload).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn the_prefix_is_the_payload_length_big_endian() {
        let msg = ClientMsg::Hello { api_version: 1 };
        let frame = encode_frame(&msg).unwrap();

        let (prefix, payload) = frame.split_at(FRAME_PREFIX_LEN);
        assert_eq!(prefix, (payload.len() as u32).to_be_bytes());
    }

    #[test]
    fn a_prefix_over_the_cap_is_refused() {
        let over = (MAX_FRAME_BYTES as u32) + 1;
        let err = payload_len(over.to_be_bytes()).unwrap_err();
        assert!(matches!(err, FrameError::TooLarge { len } if len == over as usize));
    }

    #[test]
    fn a_prefix_at_the_cap_is_allowed() {
        let at_cap = MAX_FRAME_BYTES as u32;
        let len = payload_len(at_cap.to_be_bytes()).unwrap();
        assert_eq!(len, MAX_FRAME_BYTES);
    }

    #[test]
    fn ensure_within_cap_holds_on_both_sides() {
        assert!(ensure_within_cap(MAX_FRAME_BYTES).is_ok());

        let over = MAX_FRAME_BYTES + 1;
        let err = ensure_within_cap(over).unwrap_err();
        assert!(matches!(err, FrameError::TooLarge { len } if len == over));
    }

    #[test]
    fn decoding_invalid_json_returns_a_decode_error() {
        let err = decode_payload::<ClientMsg>(b"not json").unwrap_err();
        assert!(matches!(err, FrameError::Decode(_)));
    }
}
