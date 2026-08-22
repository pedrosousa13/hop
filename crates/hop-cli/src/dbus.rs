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
    launcher_is_resident_at(&addresses)
}

/// [`launcher_is_resident`] against an explicit address list — the seam
/// the live-bus test drives, so it can point at its own private
/// `dbus-daemon` without mutating this process's environment.
fn launcher_is_resident_at(addresses: &str) -> Result<bool, DbusError> {
    let mut stream = connect_session_bus(addresses)?;
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

    let reply = read_reply(&mut stream)?;
    match reply.kind {
        MessageKind::MethodReturn => parse_boolean_body(&reply.body)
            .ok_or_else(|| DbusError::Protocol("NameHasOwner reply was not a boolean".into())),
        MessageKind::Error => Err(DbusError::Protocol(format!(
            "the bus rejected NameHasOwner(\"{LAUNCHER_BUS_NAME}\")"
        ))),
        // `read_reply` never hands back a signal; this arm exists for
        // exhaustiveness and says so rather than guessing.
        MessageKind::Signal => {
            unreachable!("read_reply discards signals and never returns one")
        }
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
#[expect(
    unsafe_code,
    reason = "the SASL EXTERNAL identity is the process's real uid, and \
              libc::getuid is the only way to read it; a plain syscall \
              wrapper with no preconditions on process state"
)]
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
    let reply = read_reply(stream)?;
    match reply.kind {
        MessageKind::MethodReturn => Ok(()),
        MessageKind::Error => Err(DbusError::Protocol("the bus rejected Hello".into())),
        // Exhaustiveness only — `read_reply` never returns a signal.
        MessageKind::Signal => unreachable!("read_reply discards signals"),
    }
}

/// The reply kinds this client acts on, plus the broadcasts it skips; a
/// client that has sent exactly one sequential call can only be awaiting
/// its reply or a signal it must ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageKind {
    MethodReturn,
    Error,
    /// A broadcast (`SIGNAL`, type 4). Never the answer to a call — a
    /// real bus interleaves them freely (`NameAcquired` arrives right
    /// after `Hello`) — so [`read_reply`] skips them instead of failing.
    Signal,
}

/// Reads messages until a reply is due, discarding the broadcast signals
/// a live bus delivers between call and reply. Calls here are strictly
/// sequential — one outstanding serial at a time — so "the next
/// non-signal message" is the answer to whichever call is in flight; a
/// client that pipelines would need `reply_serial` matching instead.
fn read_reply(stream: &mut UnixStream) -> Result<Reply, DbusError> {
    loop {
        let reply = read_message(stream)?;
        if !matches!(reply.kind, MessageKind::Signal) {
            return Ok(reply);
        }
    }
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
        // The spec's message-type constants: METHOD_RETURN = 2, ERROR = 3,
        // SIGNAL = 4. (The mapping below originally read 3/4 for the two
        // replies — off by one, invisible to every offline test and fatal
        // against a real bus, whose replies were all rejected as
        // "unexpected message type 2".) A signal parses like any other
        // message; [`read_reply`] is what discards it.
        2 => MessageKind::MethodReturn,
        3 => MessageKind::Error,
        4 => MessageKind::Signal,
        other => {
            return Err(DbusError::Protocol(format!(
                "unexpected message type {other} where a reply was due"
            )));
        }
    };
    let be = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().expect("fixed header"));
    let body_len = be(4) as usize;
    let fields_len = be(12) as usize;
    // The body begins at the next 8-byte boundary after the fields array,
    // which itself starts only after the 16-byte fixed header — both
    // offsets count from the message's first byte, so what follows the
    // header is (padded fields) + body, and the body is the tail.
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
/// 16-byte fixed header, then the array of (field-code, variant) structs —
/// PATH, DESTINATION, INTERFACE, MEMBER, and, when a body exists, the
/// body's SIGNATURE as field code 8 — then the body itself, 8-aligned
/// after the array. The body signature is a *header field*, not loose
/// bytes between array and body: writing it there instead (the shape this
/// function shipped with) leaves the body starting unaligned and the bus
/// disconnects on the first call that carries arguments.
fn marshal_method_call(
    serial: u32,
    path: &str,
    interface: &str,
    member: &str,
    body_signature: &str,
    body: &[u8],
) -> Vec<u8> {
    let mut fields = Vec::new();
    // Each field is a struct (BYTE code, VARIANT value), and a struct
    // begins on an 8-byte boundary: pad BEFORE the code byte, so the
    // struct itself — not whatever follows it — lands aligned. Padding
    // after the code byte instead is what a real bus disconnects over:
    // the misaligned array reads as garbage and the daemon drops the
    // connection mid-exchange.
    //
    // The trailing number is the value's alignment: 4 for the string-like
    // PATH/DESTINATION/INTERFACE/MEMBER, 1 for a SIGNATURE (its bytes
    // follow the length byte directly). Alignment is counted from the
    // start of the message like everything else; the fields array opens
    // at absolute offset 16, already 8-aligned, so offset-within-array
    // equals absolute offset.
    let mut fields_to_send: Vec<(u8, &str, &str, usize)> = vec![
        (1u8, "o", path, 4),                   // PATH
        (6u8, "s", "org.freedesktop.DBus", 4), // DESTINATION
        (2u8, "s", interface, 4),              // INTERFACE
        (3u8, "s", member, 4),                 // MEMBER
    ];
    if !body_signature.is_empty() {
        fields_to_send.push((8u8, "g", body_signature, 1)); // SIGNATURE
    }
    for (code, signature, value, alignment) in fields_to_send {
        while fields.len() % 8 != 0 {
            fields.push(0);
        }
        fields.push(code);
        push_signature(signature, &mut fields);
        while fields.len() % alignment != 0 {
            fields.push(0);
        }
        if alignment == 1 {
            push_signature(value, &mut fields);
        } else {
            marshal_string(value, &mut fields);
        }
    }

    let mut message = Vec::with_capacity(16 + fields.len() + body.len());
    message.push(b'l'); // little-endian
    message.push(1); // METHOD_CALL
    message.push(0); // flags: none
    message.push(1); // protocol version
    message.extend_from_slice(&(body.len() as u32).to_le_bytes());
    message.extend_from_slice(&serial.to_le_bytes());
    message.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    message.extend_from_slice(&fields);
    while message.len() % 8 != 0 {
        message.push(0);
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

/// Parses a NameHasOwner reply body as a boolean, in either shape a real
/// bus can deliver it: a bare `b` — the reply's out-argument type is
/// `b` directly, so dbus-daemon sends exactly one aligned u32 (the shape
/// only a live exchange reveals; the variant assumption below was this
/// file's third real-bus-only failure) — or a `v` wrapping one: signature
/// string `"b"`, padding to the boolean's 4-alignment, then the u32.
/// Returns `None` for anything else — a mismatched reply is the caller's
/// protocol error to report, not a default to invent.
fn parse_boolean_body(body: &[u8]) -> Option<bool> {
    if let &[a, b, c, d] = body {
        return Some(u32::from_le_bytes([a, b, c, d]) != 0);
    }
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
        // The body signature rides in the header fields (field code 8)...
        let fields_len = u32::from_le_bytes(message[12..16].try_into().unwrap()) as usize;
        let fields = &message[16..16 + fields_len];
        let sig_field = fields
            .windows(2)
            .position(|w| w == [8u8, 1])
            .expect("a body-carrying call must carry its SIGNATURE header field");
        assert_eq!(fields[sig_field + 2], b'g', "signature text");
        // ...and the body itself starts on the 8-boundary after the array.
        let body_start = (16 + fields_len + 7) & !7;
        let arg = &message[body_start..];
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
        assert_eq!(parse_boolean_body(&[1, b'b', 0, 0, 1, 0, 0, 0]), Some(true));
        assert_eq!(
            parse_boolean_body(&[1, b'b', 0, 0, 0, 0, 0, 0]),
            Some(false)
        );
        assert_eq!(parse_boolean_body(&[1, b's', 0, 0, 0, 0, 0, 0]), None);
        // The bare shape a real bus actually sends for NameHasOwner.
        assert_eq!(parse_boolean_body(&[0, 0, 0, 0]), Some(false));
        assert_eq!(parse_boolean_body(&[1, 0, 0, 0]), Some(true));
        assert_eq!(parse_boolean_body(&[0, 0, 0]), None);
        assert_eq!(parse_boolean_body(&[]), None);
    }

    /// Walks a marshalled message's header-fields array the way the spec
    /// says a bus must: each (code, variant) struct starts on an
    /// 8-boundary, and inside each variant the value sits on its own
    /// type's alignment. Returns the fields' total length so the caller
    /// can check the walk consumed exactly the array.
    fn walked_fields_len(message: &[u8]) -> usize {
        let fields_len = u32::from_le_bytes(message[12..16].try_into().unwrap()) as usize;
        let mut at = 16usize;
        let end = 16 + fields_len;
        while at < end {
            // Between one field's value and the next struct sit the
            // alignment bytes the marshalling inserts; a struct begins
            // on an 8-boundary, so skip to it before each check.
            at = (at + 7) & !7;
            let code = message[at];
            let sig_len = message[at + 1] as usize;
            let field_sig = &message[at + 2..at + 2 + sig_len];
            // The variant's signature is length byte, bytes, NUL; the
            // value follows, padded to its own alignment.
            let after_sig = at + 2 + sig_len + 1;
            let value_at = if field_sig == b"g" {
                after_sig
            } else {
                (after_sig + 3) & !3
            };
            assert_eq!(
                value_at % if field_sig == b"g" { 1 } else { 4 },
                0,
                "variant value for field code {code} at {value_at} is not aligned"
            );
            // A SIGNATURE value is one length BYTE; string-like values
            // carry a u32 length. Both are followed by their bytes and a
            // NUL.
            let str_len = if field_sig == b"g" {
                message[value_at] as usize
            } else {
                u32::from_le_bytes(message[value_at..value_at + 4].try_into().unwrap()) as usize
            };
            at = if field_sig == b"g" {
                value_at + 1 + str_len + 1
            } else {
                value_at + 4 + str_len + 1
            };
        }
        assert_eq!(at, end, "the field walk must consume exactly the array");
        fields_len
    }

    #[test]
    fn every_header_field_struct_starts_on_an_eight_byte_boundary() {
        // The regression this file's real-bus failures earned: both calls
        // this client sends, walked byte-by-byte against the alignment
        // rules. The original marshalling padded AFTER each struct's
        // code byte, left the variant's string value unaligned, appended
        // the body signature as loose bytes instead of header field 8,
        // and mapped reply types off by one — every one invisible to the
        // coarse shape assertions and fatal against a stock bus.
        let hello = marshal_method_call(
            1,
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            "",
            &[],
        );
        walked_fields_len(&hello);

        let mut body = Vec::new();
        marshal_string(LAUNCHER_BUS_NAME, &mut body);
        let name_has_owner = marshal_method_call(
            2,
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            "s",
            &body,
        );
        walked_fields_len(&name_has_owner);

        // RequestName carries a two-argument body ("su") — exercise a
        // multi-type body signature through the same walk.
        let mut body = Vec::new();
        marshal_string(LAUNCHER_BUS_NAME, &mut body);
        body.extend_from_slice(&0u32.to_le_bytes());
        let request_name = marshal_method_call(
            3,
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            "su",
            &body,
        );
        walked_fields_len(&request_name);
    }

    /// A spawned private `dbus-daemon`, killed on drop — the same shape
    /// `hop-hotkeyd`'s e2e `SessionBus` uses; duplicated rather than shared
    /// because integration-test helpers are private to their own crate.
    struct LiveBus {
        child: std::process::Child,
        address: String,
    }

    impl LiveBus {
        fn start() -> Option<Self> {
            let mut child = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?; // absent daemon = skip, per this suite's convention
            // One line: `--print-address=1` writes the address and keeps
            // running, so a read-to-EOF here would block forever — the
            // same reason `hop-hotkeyd`'s e2e reads a single line.
            let mut address = String::new();
            use std::io::Read;
            let mut byte = [0u8; 1];
            loop {
                let read = child
                    .stdout
                    .as_mut()
                    .unwrap()
                    .read(&mut byte)
                    .expect("reading dbus-daemon's address");
                if read == 0 {
                    // The daemon died before printing an address (a
                    // minimal container with no machine-id or session
                    // config, say) — an environment gap, so skip rather
                    // than fail.
                    println!("skipping: dbus-daemon exited before printing an address");
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                if byte[0] == b'\n' {
                    break;
                }
                address.push(byte[0] as char);
            }
            Some(LiveBus { child, address })
        }
    }

    impl Drop for LiveBus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    fn answers_name_has_owner_over_a_live_session_bus() {
        let Some(bus) = LiveBus::start() else {
            println!("skipping: dbus-daemon not found on $PATH");
            return;
        };
        let first = launcher_is_resident_at(&bus.address);
        assert!(
            matches!(first, Ok(false)),
            "a fresh bus should answer no residency: {first:?}"
        );

        // A second connection takes the name (RequestName, flags 0 =
        // default), speaking this same wire format through the same
        // helpers — then residency flips true. The connection must stay
        // OPEN for the name to stay owned: a bus releases the well-known
        // names of a connection the moment it disconnects, so `stream`
        // is deliberately still alive when the final check runs.
        let mut stream = connect_session_bus(&bus.address).unwrap();
        authenticate(&mut stream).unwrap();
        hello(&mut stream).unwrap();
        let mut body = Vec::new();
        marshal_string(LAUNCHER_BUS_NAME, &mut body);
        // The `u` of "su" carries its own 4-alignment: pad after the
        // string before the flags word.
        while body.len() % 4 != 0 {
            body.push(0);
        }
        body.extend_from_slice(&0u32.to_le_bytes());
        let request = marshal_method_call(
            2,
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            "su",
            &body,
        );
        stream.write_all(&request).unwrap();
        stream.flush().unwrap();
        let reply = read_reply(&mut stream).unwrap();
        assert_eq!(reply.kind, MessageKind::MethodReturn, "RequestName refused");

        let second = launcher_is_resident_at(&bus.address);
        assert!(
            matches!(second, Ok(true)),
            "after RequestName the name should be owned: {second:?}"
        );
    }
}
