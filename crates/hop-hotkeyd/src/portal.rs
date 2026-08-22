//! The GlobalShortcuts portal backend (issue #235, acceptance criterion 1):
//! the client half of `org.freedesktop.portal.GlobalShortcuts` — probe the
//! session bus for the portal, bind the configured shortcut, and block on
//! the session's `Activated` signal, running the same universal toggle the
//! X11 grab loop runs.
//!
//! # Why zbus's blocking API
//!
//! The same no-tokio argument `run.rs`'s module doc makes, one backend
//! over: this daemon waits on one signal stream and dispatches one
//! subprocess per activation. zbus's blocking facade (`zbus::blocking`)
//! runs over the executor zbus carries internally — no async runtime is
//! added to *this* crate's code, which stays the synchronous shape §3's
//! salvage manifest calls for.
//!
//! # The protocol slice this module speaks
//!
//! Exactly what the spec requires and nothing more:
//!
//! ```text
//! NameHasOwner("org.freedesktop.portal.Desktop")     — the probe
//! CreateSession({handle_token, session_handle_token}) → request handle
//!     ↳ wait Request.Response(0) on it; results carry `session_handle`
//! BindShortcuts(session, [("hop-toggle", {})], "", {handle_token})
//!     ↳ wait Request.Response(0) on it
//! block on GlobalShortcuts.Activated(session, id, t, a{sv}) → toggle
//! ```
//!
//! The client never constructs portal object paths itself: the request
//! handle comes back from each method call and the session handle comes
//! back inside the first `Response`'s results, so the sender-scoped path
//! spelling stays the portal's business, not ours.
//!
//! # Degradation posture (issue #235's criterion 3)
//!
//! Every failure here is a `Result<_, String>` carrying a printed reason —
//! no portal name on the bus, a refused bind, a lost connection — and the
//! caller (`run.rs`'s backend selection) logs it and falls through to the
//! next backend in the documented order. Nothing here panics, and nothing
//! fails silently.
//!
//! # What the headless tests cannot prove
//!
//! `tests/portal.rs` serves a fake portal on a private session bus and
//! proves this module's round trip — probe, bind, synthetic `Activated`,
//! toggle spawned — end to end against the real binary. What no headless
//! test can prove is the real-portal remainder: actual xdg-desktop-portal
//! implementations, real DE confirmation dialogs, and real desktop
//! behaviour on KDE and GNOME 48+. That remainder is explicitly left to
//! the manual verification pass.
//!
//! # Timing
//!
//! [`RESPONSE_TIMEOUT`] is deliberately generous: a real desktop's
//! `BindShortcuts` may sit behind a user-facing confirmation dialog, and
//! falling back to X11 while the user is still deciding would steal the
//! keybinding out from under them. The headless fake answers in
//! microseconds; the timeout exists so a wedged portal degrades instead of
//! hanging the daemon forever.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Message;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

/// The well-known name every freedesktop portal lives behind. The
/// GlobalShortcuts interface has no name of its own — probing it means
/// asking the bus whether *this* name has an owner (the `NameHasOwner`
/// call `hop-cli/src/dbus.rs` hand-rolls for hop-gtk's residency; here
/// zbus asks it for us).
const PORTAL_SERVICE: &str = "org.freedesktop.portal.Desktop";

/// The object path the desktop portal serves every portal interface from.
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

/// The portal interface this module binds through and listens on.
const GLOBALSHORTCUTS_IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";

/// The per-request callback interface every portal method reply is
/// completed by: the method returns a request handle immediately, and the
/// actual verdict arrives later as `Response(response_code, results)` on
/// that handle.
const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";

/// How long to wait for a portal's `Response` before declaring the backend
/// unavailable and falling through — see this module's "Timing".
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

/// The shortcut id hop binds. One id, because the config's `toggle`
/// bindings are alternative spellings of one action; the id is what the
/// portal's `Activated` signal names back to us.
const SHORTCUT_ID: &str = "hop-toggle";

/// How long any single portal exchange may take — named once so the two
/// call sites and the timeout error message cannot drift apart. Generous
/// because a real DE may put a confirmation dialog in front of
/// `BindShortcuts`; see this module's "Timing".
///
/// Everything [`probe`] and [`bind`] can conclude. Every variant is a
/// printable reason: backend selection logs it and falls through, never
/// panics, never fails silently (issue #235's criterion 3).
pub type PortalError = String;

/// A bound portal session: the live bus connection plus the session handle
/// the portal assigned, which the `Activated` signal arrives on.
pub struct PortalSession {
    conn: Connection,
    session_handle: OwnedObjectPath,
}

/// Asks the session bus whether the desktop portal is there at all.
///
/// Two independent ways to say no, both returned as reasons: no session
/// bus to ask (headless CI without one, a broken `$DBUS_SESSION_BUS_ADDRESS`)
/// and a bus with no portal service on it (most X11 sessions, every CI
/// runner). Either way the caller falls through to the next backend.
pub fn probe() -> Result<Connection, PortalError> {
    let conn = Connection::session().map_err(|err| format!("no session bus ({err})"))?;
    let owned: bool = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &(PORTAL_SERVICE,),
        )
        .map_err(|err| format!("cannot query the session bus ({err})"))?
        .body()
        .deserialize::<bool>()
        .map_err(|err| format!("the session bus answered gibberish ({err})"))?;
    if owned {
        Ok(conn)
    } else {
        Err(format!(
            "no service owns {PORTAL_SERVICE} on the session bus"
        ))
    }
}

/// Runs the portal handshake for `bindings`: `CreateSession`, then
/// `BindShortcuts` for every configured toggle binding under one shortcut
/// id. Returns the live session, or the reason the portal refused — which
/// the caller logs before falling through to the next backend.
pub fn bind(
    conn: &Connection,
    bindings: &[crate::config::ToggleEntry],
) -> Result<PortalSession, PortalError> {
    add_signal_matches(conn)?;
    let session_handle = create_session(conn)?;
    // The shortcuts array is `a(sa{sv})`: one entry per configured binding,
    // each carrying the id the portal will name in `Activated` and an empty
    // property dict — description and icon are portal-UI garnish this
    // daemon has no use for.
    let shortcuts: Vec<(String, HashMap<String, Value<'static>>)> = bindings
        .iter()
        .map(|_| (SHORTCUT_ID.to_string(), HashMap::new()))
        .collect();
    // `a{sv}` — a dict, which is what a HashMap serializes as (a plain
    // slice of pairs would marshal as `a(sv)`, a different D-Bus type).
    let mut options: HashMap<&str, Value<'static>> = HashMap::new();
    options.insert("handle_token", handle_token_value());
    // The watcher is armed *before* the call: the portal's `Response` can
    // arrive the moment the method is dispatched, and a subscription opened
    // after the reply could miss it.
    let watcher = response_watcher(conn);
    let reply = conn
        .call_method(
            Some(PORTAL_SERVICE),
            PORTAL_PATH,
            Some(GLOBALSHORTCUTS_IFACE),
            "BindShortcuts",
            &(&session_handle, &shortcuts, "", &options),
        )
        .map_err(|err| format!("BindShortcuts failed ({err})"))?;
    let request = reply_body_path(&reply, "BindShortcuts")?;
    let (code, _results) = wait_response(&watcher, &request)?;
    if code != 0 {
        return Err(format!("BindShortcuts refused (response code {code})"));
    }
    Ok(PortalSession {
        conn: conn.clone(),
        session_handle,
    })
}

/// Blocks forever on the session's `Activated` signal, running `toggle`
/// once per activation. Returns only when the stream itself breaks — the
/// portal or the bus went away — with the reason; the caller decides what
/// degradation that means. Signals for other sessions or other portal
/// methods on the same connection are ignored.
pub fn serve(session: PortalSession, mut toggle: impl FnMut()) -> Result<(), PortalError> {
    // The whole-connection iterator is subscribed before anything else so a
    // portal that fires immediately cannot outrun the subscription.
    let mut stream = MessageIterator::from(&session.conn);
    loop {
        let msg = match stream.next() {
            // zbus broadcasts the read error to armed iterators before it
            // closes their channels, so a mid-session death lands here as
            // an `Err`; `None` means the stream ended some other way.
            // Either way the answer is the same: report the loss and let
            // the caller degrade — nothing here may panic.
            Some(Ok(msg)) => msg,
            Some(Err(err)) => return Err(format!("lost the portal session ({err})")),
            None => return Err("lost the portal session (message stream ended)".to_string()),
        };
        if !is_signal_on(
            &msg,
            &session.session_handle,
            GLOBALSHORTCUTS_IFACE,
            "Activated",
        ) {
            continue;
        }
        // Body: (o session_handle, s shortcut_id, t timestamp, a{sv} options).
        // The handle and timestamp name the activation; only the fact of it
        // matters here, so they are consumed and dropped.
        let (_handle, id, _timestamp, _options): (
            OwnedObjectPath,
            String,
            u64,
            HashMap<String, OwnedValue>,
        ) = msg
            .body()
            .deserialize()
            .map_err(|err| format!("malformed Activated signal ({err})"))?;
        log(&format!("portal shortcut `{id}` activated"));
        toggle();
    }
}

/// `CreateSession` and its `Response` wait, folded together: the session
/// handle comes back not from the method reply (which carries only the
/// request handle) but from the `Response`'s results dict.
fn create_session(conn: &Connection) -> Result<OwnedObjectPath, PortalError> {
    let mut options: HashMap<&str, Value<'static>> = HashMap::new();
    options.insert("handle_token", handle_token_value());
    // The token the portal folds into the session path; fixed because this
    // daemon ever creates exactly one session per run.
    options.insert("session_handle_token", Value::from("hop"));
    // Same arm-before-call ordering as `bind` — see the comment there.
    let watcher = response_watcher(conn);
    let reply = conn
        .call_method(
            Some(PORTAL_SERVICE),
            PORTAL_PATH,
            Some(GLOBALSHORTCUTS_IFACE),
            "CreateSession",
            &(&options,),
        )
        .map_err(|err| format!("CreateSession failed ({err})"))?;
    let request = reply_body_path(&reply, "CreateSession")?;
    let (code, results) = wait_response(&watcher, &request)?;
    if code != 0 {
        return Err(format!("CreateSession refused (response code {code})"));
    }
    // The portal folds the token into a sender-scoped path; whatever
    // spelling it chose comes back as a string and only ever gets echoed.
    let handle = results
        .get("session_handle")
        .ok_or_else(|| "CreateSession's Response carried no session_handle".to_string())?;
    let handle = String::try_from(handle.clone())
        .map_err(|err| format!("session_handle is not a string ({err})"))?;
    let path = OwnedObjectPath::try_from(handle.clone())
        .map_err(|err| format!("session_handle `{handle}` is not an object path ({err})"))?;
    Ok(path)
}

/// One `Response` verdict as the watcher forwards it: which request handle
/// it completed, the portal's response code (0 = accepted), and the
/// results dict (`CreateSession`'s carries the session handle).
type ResponseMsg = Result<(OwnedObjectPath, u32, HashMap<String, OwnedValue>), PortalError>;

/// Arms a whole-connection signal watcher on a worker thread, forwarding
/// every `org.freedesktop.portal.Request.Response` it sees.
///
/// Arming happens *before* the caller issues its method call — the
/// subscription must predate the reply, because a fast portal completes
/// its request while the method call is still in flight. zbus delivers
/// every incoming message to every live iterator, so this watcher and any
/// later listener never steal from each other. The thread is abandoned on
/// timeout: it holds one connection clone and dies with the process, the
/// right price for never blocking the fallback path on a wedged portal.
fn response_watcher(conn: &Connection) -> mpsc::Receiver<ResponseMsg> {
    let (tx, rx) = mpsc::channel();
    // The subscription is created *here*, on the caller's thread, before
    // this function returns — and only then handed to the worker. Spawning
    // the thread first would leave a scheduling window in which the call
    // is already in flight but the subscription does not exist yet, and a
    // fast portal's Response (the common case: a fake answers before the
    // thread is even scheduled; a real one answers its non-interactive
    // requests in microseconds) would land in that window and be lost.
    let mut stream = MessageIterator::from(conn);
    std::thread::spawn(move || {
        loop {
            let Some(msg) = stream.next() else {
                break;
            };
            let Ok(msg) = msg else {
                let _ = tx.send(Err("the session bus went away".to_string()));
                return;
            };
            if !is_signal(&msg, REQUEST_IFACE, "Response") {
                continue;
            }
            let code_results: Result<(u32, HashMap<String, OwnedValue>), PortalError> = msg
                .body()
                .deserialize()
                .map_err(|err| format!("malformed Response signal ({err})"));
            let path: Option<OwnedObjectPath> = msg
                .header()
                .path()
                .map(|p| OwnedObjectPath::from(p.to_owned()));
            let forwarded = match (path, code_results) {
                (Some(path), Ok((code, results))) => Ok((path, code, results)),
                (_, Err(reason)) => Err(reason),
                (None, Ok(_)) => Err("Response signal carried no path".to_string()),
            };
            if tx.send(forwarded).is_err() {
                return; // caller gave up (timeout); nothing left to watch for
            }
        }
    });
    rx
}

/// Waits on an armed [`response_watcher`] for the `Response` that
/// completes `request`, with the generous [`RESPONSE_TIMEOUT`] — forward
/// verdicts for other requests are skipped, not consumed as ours.
fn wait_response(
    rx: &mpsc::Receiver<ResponseMsg>,
    request: &ObjectPath,
) -> Result<(u32, HashMap<String, OwnedValue>), PortalError> {
    let deadline = std::time::Instant::now() + RESPONSE_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let (path, code, results) = rx.recv_timeout(remaining).map_err(|_| {
            format!(
                "timed out after {}s waiting for the portal's Response on {request}",
                RESPONSE_TIMEOUT.as_secs()
            )
        })??;
        if path.as_str() == request.as_str() {
            return Ok((code, results));
        }
    }
}

/// Extracts the request-handle object path a portal method call returns.
fn reply_body_path(reply: &Message, method: &str) -> Result<OwnedObjectPath, PortalError> {
    reply
        .body()
        .deserialize::<OwnedObjectPath>()
        .map_err(|err| format!("{method}'s reply carried no handle ({err})"))
}

/// Whether `msg` is the named signal from the named interface, optionally
/// constrained to one object path — the one filter every wait in this
/// module applies (`serve` pins the session handle; the response watcher
/// forwards every request's verdict and lets [`wait_response`] match).
fn is_signal(msg: &Message, interface: &str, member: &str) -> bool {
    let header = msg.header();
    header.interface().is_some_and(|iface| iface == interface)
        && header.member().is_some_and(|name| name == member)
}

/// Same as [`is_signal`], with the object path also required.
fn is_signal_on(msg: &Message, path: &ObjectPath, interface: &str, member: &str) -> bool {
    msg.header().path() == Some(path) && is_signal(msg, interface, member)
}

/// Registers this connection's interest in the two signal streams the
/// protocol rides on. Required, not optional polish: a D-Bus *bus* delivers
/// broadcast signals only to connections holding a matching match rule
/// (method replies are unicast and arrive regardless — which is exactly why
/// an unexplained silence here would be so confusing). Both rules cover the
/// whole interface, so they are added once per connection, up front.
fn add_signal_matches(conn: &Connection) -> Result<(), PortalError> {
    for rule in [
        format!("type='signal',interface='{REQUEST_IFACE}'"),
        format!("type='signal',interface='{GLOBALSHORTCUTS_IFACE}'"),
    ] {
        conn.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "AddMatch",
            &(rule.as_str(),),
        )
        .map_err(|err| format!("cannot register signal interest ({err})"))?;
    }
    Ok(())
}

/// The `handle_token` every portal call carries: the portal folds it into
/// the request object path, so it must be a path-safe token. Numbered by
/// call order via a counter so concurrent calls cannot collide.
fn handle_token_value() -> Value<'static> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let token = format!("hop_{}", NEXT.fetch_add(1, Ordering::Relaxed));
    Value::from(token)
}

/// The one logging spelling this crate uses — bare stderr, the workspace's
/// established convention, with the greppable `hop-hotkeyd:` prefix
/// `hop doctor`'s M6 report will consume.
fn log(line: &str) {
    eprintln!("hop-hotkeyd: {line}");
}
