//! A minimal D-Bus *client*, hand-rolled down to the wire format, answering
//! exactly one question: does the session bus own a given well-known name?
//!
//! # Why this exists, and why it is not zbus (issue #234)
//!
//! `hop toggle`'s residency check (criterion 3) needs one method call —
//! `org.freedesktop.DBus.NameHasOwner` — answered over the session bus.
//! The obvious library answer is `zbus`, and #235 will likely bring it for
//! the portal backends, which need real marshalling breadth (variants,
//! dicts, signals, async). This binary is the opposite of that shape: the
//! same "one socket, one exchange, strictly sequential" process its module
//! doc refuses to give a tokio runtime to would gain zbus's whole dependency
//! tree — an async executor, futures plumbing, serde machinery — to ask a
//! yes/no question once and exit. The full protocol this module speaks is,
//! deliberately, the smallest slice that does that honestly: SASL
//! `EXTERNAL` auth, `Hello`, one method call, one reply. Anything more
//! (activating names, receiving signals, the portals) is exactly where this
//! module stops and zbus begins — if `toggle` ever grows beyond
//! `NameHasOwner`, that is the signal to delete this file and take the
//! dependency.
//!
//! # Why the residency check at all
//!
//! Without it, `hop toggle` with nothing running would *launch* hop-gtk as
//! a fresh primary instance — indistinguishable from activating a resident
//! one from the outside, and wrong twice over: criterion 3 demands a plain
//! refusal with non-zero exit, and §3's model has hop-gtk started
//! deliberately (autostart, systemd user unit), not implicitly by the first
//! stray keypress.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;

/// hop-gtk's well-known bus name (`app.rs`'s `APP_ID`) — the thing whose
/// presence on the session bus means "a launcher is resident".
pub const LAUNCHER_BUS_NAME: &str = "dev.hop.Launcher";

/// The one method call this module makes: `NameHasOwner` takes a single
/// string argument and answers with a single boolean, in a variant.
pub const NAME_HAS_OWNER_CALL: (&str, &str, &str) = (
    "/org/freedesktop/DBus",
    "org.freedesktop.DBus",
    "NameHasOwner",
);

/// Every way asking the bus our one question can fail.
#[derive(Debug)]
pub enum DbusError {
    /// `$DBUS_SESSION_BUS_ADDRESS` is unset or empty — there is no session
    /// bus to ask, which for residency purposes means "not resident", but
    /// is reported distinctly so the caller can say *why* it could not
    /// check.
    NoSessionBus,
    /// No transport in the address list accepted a connection.
    Connect(io::Error),
    /// The socket conversation failed mid-flight.
    Io(io::Error),
    /// The bus refused our credentials or answered gibberish.
    Protocol(String),
}

impl std::fmt::Display for DbusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbusError::NoSessionBus => write!(f, "DBUS_SESSION_BUS_ADDRESS is not set"),
            DbusError::Connect(err) => write!(f, "cannot connect to the session bus: {err}"),
            DbusError::Io(err) => write!(f, "session bus I/O failed: {err}"),
            DbusError::Protocol(reason) => write!(f, "session bus protocol error: {reason}"),
        }
    }
}

/// Asks the session bus whether [`LAUNCHER_BUS_NAME`] has an owner.
///
/// A bus that cannot be reached at all is reported as an error rather than
/// folded into `Ok(false)`: "no launcher running" and "no bus to check on"
/// are different situations a user debugging their session needs told
/// apart, even though both mean the toggle cannot proceed.
pub fn launcher_is_resident() -> Result<bool, DbusError> {
    let addresses =
        std::env::var("DBUS_SESSION_BUS_ADDRESS").map_err(|_| DbusError::NoSessionBus)?;
    let mut stream = connect_session_bus(&addresses)?;
    authenticate(&mut stream)?;
    hello(&mut stream)?;

    let (path, interface, member) = NAME_HAS_OWNER_CALL;
    let mut body = Vec::new();
    marshal_string(LAUNCHER_BUS_NAME, &mut body);
    // Serial 2: `Hello` below already claimed serial 1 on this connection.
    let request = marshal_method_call(2, path, interface, member, "s", &body);
    stream
        .write_all(&request)
        .and_then(|()| stream.flush())
        .map_err(DbusError::Io)?;

    let reply = read_message(&mut stream)?;
    match reply.kind {
        MessageKind::MethodReturn => parse_variant_bool(&reply.body)
            .ok_or_else(|| DbusError::Protocol("NameHasOwner reply was not a boolean".into())),
        MessageKind::Error => Err(DbusError::Protocol(format!(
            "the bus rejected NameHasOwner(\"{LAUNCHER_BUS_NAME}\")"
        ))),
    }
}

/// Splits a `DBUS_SESSION_BUS_ADDRESS` into its transport entries and tries
/// each until one connects — the spec-mandated fallback order, so a bus
/// reachable over several transports still connects when the first entry is
/// stale.
fn connect_session_bus(addresses: &str) -> Result<UnixStream, DbusError> {
    let mut last = DbusError::NoSessionBus;
    for entry in addresses.split(';') {
        let entry = entry.trim();
        let Some(socket_path) = unix_socket_path(entry) else {
            continue;
        };
        match UnixStream::connect(&socket_path) {
            Ok(stream) => return Ok(stream),
            Err(err) => last = DbusError::Connect(err),
        }
    }
    Err(last)
}

/// Extracts the filesystem path from one `unix:path=<p>` address entry.
/// (`unix:abstract=` sockets live in the abstract namespace and are not
/// addressed by path; they are skipped rather than misdialed.)
fn unix_socket_path(entry: &str) -> Option<std::path::PathBuf> {
    let rest = entry.strip_prefix("unix:")?;
    for pair in rest.split(',') {
        if let Some(path) = pair.strip_prefix("path=") {
            return Some(std::path::PathBuf::from(path));
        }
    }
    None
}

/// Performs the SASL handshake: null-byte greeting, `AUTH EXTERNAL` with the
/// hex-encoded decimal uid (the mechanism every stock session bus accepts
/// from a same-user client), then `BEGIN` to switch to the message stream.
#[expect(unsafe_code)]
fn authenticate(stream: &mut UnixStream) -> Result<(), DbusError> {
    // SAFETY: `libc::getuid` reads the calling process's real uid — a plain
    // syscall wrapper with no preconditions on process state.
    let uid = unsafe { libc::getuid() };
    let identity = uid.to_string();
    let hex: String = identity.bytes().map(|b| format!("{b:02x}")).collect();

    stream.write_all(b"\0").map_err(DbusError::Io)?;
    stream
        .write_all(format!("AUTH EXTERNAL {hex}\r\n").as_bytes())
        .map_err(DbusError::Io)?;
    let response = read_sasl_line(stream)?;
    if !response.starts_with("OK ") {
        return Err(DbusError::Protocol(format!(
            "the bus rejected EXTERNAL auth ({response})"
        )));
    }
    stream.write_all(b"BEGIN\r\n").map_err(DbusError::Io)?;
    Ok(())
}

/// Reads one `\r\n`-terminated SASL line (the pre-`BEGIN` framing).
fn read_sasl_line(stream: &mut UnixStream) -> Result<String, DbusError> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = stream.read(&mut byte).map_err(DbusError::Io)?;
        if read == 0 {
            return Err(DbusError::Protocol(
                "the bus closed the connection during auth".into(),
            ));
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).map_err(|_| DbusError::Protocol("non-UTF-8 auth response".into()))
}

/// Sends the mandatory `Hello` and consumes its reply (the assigned unique
/// name, which this client has no further use for). The bus refuses every
/// other call from a connection that has not said hello.
fn hello(stream: &mut UnixStream) -> Result<(), DbusError> {
    let request = marshal_method_call(
        1,
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "Hello",
        "",
        &[],
    );
    stream.write_all(&request).map_err(DbusError::Io)?;
    let reply = read_message(stream)?;
    match reply.kind {
        MessageKind::MethodReturn => Ok(()),
        MessageKind::Error => Err(DbusError::Protocol("the bus rejected Hello".into())),
    }
}

/// The two reply kinds this client distinguishes; anything else on the wire
/// is a protocol error, since a client that has sent exactly two calls can
/// only be awaiting exactly these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageKind {
    MethodReturn,
    Error,
}

/// One parsed reply: its kind and its body bytes.
struct Reply {
    kind: MessageKind,
    body: Vec<u8>,
}

/// Reads one complete message — fixed 16-byte header, padded header fields,
/// body — off the stream. The header fields are counted and skipped, not
/// parsed: none of them carries information this client acts on (an ERROR's
/// error-name field is summarized by [`DbusError::Protocol`]'s fixed text
/// instead).
fn read_message(stream: &mut UnixStream) -> Result<Reply, DbusError> {
    let mut header = [0u8; 16];
    read_exact(stream, &mut header)?;
    if header[0] != b'l' {
        return Err(DbusError::Protocol(
            "the bus answered in big-endian, which this client does not speak".into(),
        ));
    }
    let kind = match header[1] {
        3 => MessageKind::MethodReturn,
        4 => MessageKind::Error,
        other => {
            return Err(DbusError::Protocol(format!(
                "unexpected message type {other} where a reply was due"
            )));
        }
    };
    let be = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().expect("fixed header"));
    let body_len = be(4) as usize;
    let fields_len = be(12) as usize;
    // The body begins at the next 8-byte boundary after the fields array.
    let fields_padded = (fields_len + 7) & !7;

    let mut rest = vec![0u8; fields_padded + body_len];
    read_exact(stream, &mut rest)?;
    Ok(Reply {
        kind,
        body: rest.split_off(fields_padded),
    })
}

/// Fills `buf` completely or fails — a short read mid-message leaves the
/// stream desynchronized, so it must surface rather than be retried blind.
fn read_exact(stream: &mut UnixStream, buf: &mut [u8]) -> Result<(), DbusError> {
    stream.read_exact(buf).map_err(|err| {
        if err.kind() == io::ErrorKind::UnexpectedEof {
            DbusError::Protocol("the bus closed the connection mid-message".into())
        } else {
            DbusError::Io(err)
        }
    })
}

/// Appends one D-Bus `STRING`/`OBJECT_PATH` (same wire shape): u32 byte
/// length, UTF-8 bytes, terminating NUL, 4-aligned.
fn marshal_string(value: &str, out: &mut Vec<u8>) {
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
    out.push(0);
}

/// Builds a complete little-endian METHOD_CALL message.
///
/// `body_signature` is the argument signature (`""` or `"s"` here) and
/// `body` the pre-marshalled arguments. Header layout, per the spec: the
/// 16-byte fixed header, then the 8-aligned array of (field-code, variant)
/// pairs carrying PATH, DESTINATION, INTERFACE and MEMBER, then the optional
/// body signature and body at the next 8-boundary.
fn marshal_method_call(
    serial: u32,
    path: &str,
    interface: &str,
    member: &str,
    body_signature: &str,
    body: &[u8],
) -> Vec<u8> {
    // Each field: 1 byte code + padding to 8 + signature string + value.
    let mut fields = Vec::new();
    for (code, signature, value) in [
        (1u8, "o", path),                   // PATH
        (6u8, "s", "org.freedesktop.DBus"), // DESTINATION
        (2u8, "s", interface),              // INTERFACE
        (3u8, "s", member),                 // MEMBER
    ] {
        fields.push(code);
        while fields.len() % 8 != 0 {
            fields.push(0);
        }
        push_signature(signature, &mut fields);
        marshal_string(value, &mut fields);
    }

    let mut message = Vec::with_capacity(16 + fields.len() + body.len());
    message.push(b'l'); // little-endian
    message.push(1); // METHOD_CALL
    message.push(0); // flags: none
    message.push(1); // protocol version
    message.extend_from_slice(&(body.len() as u32).to_le_bytes());
    message.extend_from_slice(&serial.to_le_bytes());
    message.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    while message.len() % 8 != 0 {
        message.push(0);
    }
    message.extend_from_slice(&fields);
    while message.len() % 8 != 0 {
        message.push(0);
    }
    if !body_signature.is_empty() {
        push_signature(body_signature, &mut message);
    }
    message.extend_from_slice(body);
    message
}

/// Appends a signature value: length byte, characters, NUL. (A signature is
/// the one string-like type whose bytes follow the length byte directly.)
fn push_signature(signature: &str, out: &mut Vec<u8>) {
    out.push(signature.len() as u8);
    out.extend_from_slice(signature.as_bytes());
    out.push(0);
}

/// Parses a `v` (variant) body known to wrap a boolean: the variant's
/// signature string (`"b"`), padding the boolean to its own 4-alignment,
/// then the u32 boolean itself. Returns `None` for anything else — a
/// mismatched reply is the caller's protocol error to report, not a default
/// to invent.
fn parse_variant_bool(body: &[u8]) -> Option<bool> {
    if body.first() != Some(&1) || body.get(1) != Some(&b'b') {
        return None;
    }
    let value = u32::from_le_bytes(body.get(4..8)?.try_into().ok()?);
    Some(value != 0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn address_entries_yield_their_paths_and_skip_abstract() {
        assert_eq!(
            unix_socket_path("unix:path=/run/user/1000/bus"),
            Some(std::path::PathBuf::from("/run/user/1000/bus"))
        );
        assert_eq!(unix_socket_path("unix:abstract=/tmp/dbus-x1"), None);
        assert_eq!(unix_socket_path("tcp:host=localhost,port=1"), None);
    }

    #[test]
    fn a_hello_call_marshals_to_a_well_formed_message() {
        // Golden-shape checks against the spec: little-endian METHOD_CALL,
        // empty body, serial carried through, member name present verbatim.
        let message = marshal_method_call(
            1,
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            "",
            &[],
        );
        assert_eq!(message[0], b'l');
        assert_eq!(message[1], 1, "METHOD_CALL");
        assert_eq!(&message[4..8], &0u32.to_le_bytes(), "empty body");
        assert_eq!(&message[8..12], &1u32.to_le_bytes(), "serial");
        let needle = b"Hello";
        assert!(
            message.windows(needle.len()).any(|w| w == needle),
            "member name missing from {message:?}"
        );
        // The header+fields region stays 8-aligned so the body (here absent)
        // would begin on its required boundary.
        let fields_len = u32::from_le_bytes(message[12..16].try_into().unwrap()) as usize;
        assert_eq!(((16 + fields_len + 7) & !7) % 8, 0);
    }

    #[test]
    fn a_name_has_owner_call_carries_its_argument_in_the_body() {
        let mut body = Vec::new();
        marshal_string(LAUNCHER_BUS_NAME, &mut body);
        let message = marshal_method_call(
            7,
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            "s",
            &body,
        );
        let body_len = u32::from_le_bytes(message[4..8].try_into().unwrap()) as usize;
        assert_eq!(body_len, 4 + LAUNCHER_BUS_NAME.len() + 1);
        // Body signature "s" sits right after the padded fields...
        let fields_len = u32::from_le_bytes(message[12..16].try_into().unwrap()) as usize;
        let body_start = (16 + fields_len + 7) & !7;
        assert_eq!(message[body_start], 1, "signature length");
        assert_eq!(message[body_start + 1], b's');
        // ...followed by the argument string itself.
        let arg = &message[body_start + 3..];
        assert_eq!(
            u32::from_le_bytes(arg[..4].try_into().unwrap()) as usize,
            LAUNCHER_BUS_NAME.len()
        );
        assert!(arg[4..].starts_with(LAUNCHER_BUS_NAME.as_bytes()));
    }

    #[test]
    fn variant_bool_bodies_parse_both_ways_and_refuse_others() {
        // Signature "b" (len 1, 'b', NUL), pad to the bool's 4-alignment,
        // then the u32 boolean.
        assert_eq!(parse_variant_bool(&[1, b'b', 0, 0, 1, 0, 0, 0]), Some(true));
        assert_eq!(
            parse_variant_bool(&[1, b'b', 0, 0, 0, 0, 0, 0]),
            Some(false)
        );
        assert_eq!(parse_variant_bool(&[1, b's', 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(parse_variant_bool(&[]), None);
    }
}
