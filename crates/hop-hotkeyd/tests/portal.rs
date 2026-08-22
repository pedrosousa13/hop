//! The headless proof issue #235's criterion 1 asks for: against a fake
//! GlobalShortcuts portal served *by this test process* on a private
//! session bus, the real `hop-hotkeyd` binary probes the portal, binds the
//! configured shortcut, and fires the universal toggle when the synthetic
//! `Activated` arrives — plus the two degradation arms of criterion 3
//! (no portal at all; a portal that refuses the bind).
//!
//! # How "toggle fired" is observed
//!
//! [`run.rs`](../src/run.rs)'s `spawn_toggle` execs `hop toggle` from
//! `$PATH`, so each test rigs `PATH` with a stub directory whose `hop`
//! appends a marker line to a temp file — observation without touching the
//! toggle side at all.
//!
//! # What the fake serves
//!
//! Exactly the protocol slice `src/portal.rs` speaks, on the paths the
//! portal spec prescribes: `CreateSession`/`BindShortcuts` methods on
//! `/org/freedesktop/portal/desktop`, each completed by a
//! `org.freedesktop.portal.Request.Response` signal on the request handle
//! (code 0 accepted / 1 refused, `session_handle` in the first reply's
//! results dict), and the `Activated` signal on the session handle. The
//! sender-scoped segment real portals fold into request/session paths is
//! spelled `fake` here — the client treats those paths as opaque, which is
//! part of what the round trip proves.
//!
//! # What no headless test can prove
//!
//! This file proves hop's half of the protocol. It cannot prove the
//! real-portal remainder — actual xdg-desktop-portal implementations, real
//! DE confirmation dialogs, KDE/GNOME 48+ behaviour — which is explicitly
//! left to the manual verification pass (the same disclaimer
//! `src/portal.rs`'s module doc carries).
//!
//! # Skip posture
//!
//! Like `e2e.rs`: every prerequisite is checked up front, and a missing
//! one skips the test with a printed reason rather than failing — but CI
//! provisions `dbus`, where these run as hard requirements. Unlike
//! `e2e.rs` no Xvfb is needed: the fallback arms *require* X11 to be
//! unreachable, so each child gets `DISPLAY` removed explicitly and the
//! guidance outcome is deterministic.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::message::Message;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// The well-known name the fake requests — what `NameHasOwner` probes.
const PORTAL_SERVICE: &str = "org.freedesktop.portal.Desktop";

/// Where the desktop portal serves its interfaces.
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
/// The GlobalShortcuts interface under test.
const GLOBALSHORTCUTS_IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";

/// The per-request callback interface.
const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";

/// The session handle this fake hands out — fixed, because the client
/// echoes whatever the portal says rather than constructing paths.
const SESSION_PATH: &str = "/org/freedesktop/portal/desktop/session/fake/hop";

/// The binding every test configures, matching `e2e.rs`.
const BINDING: &str = "ctrl+alt+space";

/// How long any single "wait for reality to reflect the change" poll may
/// run — generous on purpose, matching `e2e.rs`'s reasoning.
const POLL_TIMEOUT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Process plumbing — duplicated from tests/e2e.rs, per that file's module
// doc ("integration-test helpers are private to their own test crate").
// ---------------------------------------------------------------------------

/// A private session bus, spawned directly, address captured from stdout —
/// `e2e.rs`'s helper verbatim in shape.
struct SessionBus {
    child: Child,
    address: String,
}

impl SessionBus {
    fn start() -> Option<Self> {
        let dbus_daemon = find_in_path("dbus-daemon")?;
        let mut child = Command::new(dbus_daemon)
            .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("dbus-daemon was found on $PATH but could not be spawned");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut address = String::new();
        BufReader::new(stdout)
            .read_line(&mut address)
            .expect("reading dbus-daemon's address");
        let address = address.trim().to_string();
        if address.is_empty() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Some(SessionBus { child, address })
    }
}

impl Drop for SessionBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A spawned process killed on drop, so a failing assertion cannot leak it.
struct ChildProcess {
    child: Child,
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn poll_until<T>(context: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + POLL_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(value) = f() {
            return value;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("{context}: not observed within {}s", POLL_TIMEOUT.as_secs());
}

// ---------------------------------------------------------------------------
// The fake portal.
// ---------------------------------------------------------------------------

/// Serves `org.freedesktop.portal.GlobalShortcuts` from this test process.
///
/// Method bodies deliberately run against the *async* inner connection —
/// they execute on zbus's own executor thread, where the blocking facade's
/// internal `block_on` must not be re-entered. The test thread talks to the
/// same connection only through the blocking wrapper ([`PortalHandle`] below),
/// never concurrently with a method body.
struct FakePortal {
    /// The async inner connection this fake serves on.
    conn: zbus::Connection,
    /// Whether `BindShortcuts` should answer with refusal code 1.
    refuse_bind: Arc<AtomicBool>,
    /// Set once a bind was accepted — the round-trip test waits on this
    /// before firing the synthetic activation.
    bound_session: Arc<Mutex<Option<String>>>,
}

#[zbus::interface(name = "org.freedesktop.portal.GlobalShortcuts")]
impl FakePortal {
    async fn create_session(&self, options: HashMap<String, OwnedValue>) -> OwnedObjectPath {
        let request = request_path(&handle_token(&options));
        self.conn
            .object_server()
            .at(request.as_str(), FakeRequest)
            .await
            .expect("registering the request object");
        let mut results: HashMap<String, OwnedValue> = HashMap::new();
        results.insert(
            "session_handle".to_string(),
            OwnedValue::try_from(Value::from(SESSION_PATH)).expect("session handle variant"),
        );
        emit_response(&self.conn, &request, 0, results).await;
        OwnedObjectPath::try_from(request).expect("request path")
    }

    async fn bind_shortcuts(
        &self,
        _session_handle: OwnedObjectPath,
        _shortcuts: Vec<(String, HashMap<String, OwnedValue>)>,
        _parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> OwnedObjectPath {
        let request = request_path(&handle_token(&options));
        self.conn
            .object_server()
            .at(request.as_str(), FakeRequest)
            .await
            .expect("registering the request object");
        let accepted = !self.refuse_bind.load(Ordering::SeqCst);
        if accepted {
            // Scoped so the guard is dropped before the `await` below:
            // zbus's interface macro requires method futures to be Send.
            let mut bound = self
                .bound_session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *bound = Some(SESSION_PATH.to_string());
        }
        emit_response(
            &self.conn,
            &request,
            if accepted { 0 } else { 1 },
            HashMap::new(),
        )
        .await;
        OwnedObjectPath::try_from(request).expect("request path")
    }
}

/// The per-request callback object: carries no state, exists so the
/// `Response` signal has an interface to ride on.
struct FakeRequest;

#[zbus::interface(name = "org.freedesktop.portal.Request")]
impl FakeRequest {
    // No methods — the fake emits `Response` one-way, exactly like a real
    // portal completing a request.
}

/// The `handle_token` the caller folded into its options dict; the fake
/// mirrors the spec's rule that the token names the request path.
fn handle_token(options: &HashMap<String, OwnedValue>) -> String {
    String::try_from(
        options
            .get("handle_token")
            .unwrap_or_else(|| panic!("call carried no handle_token"))
            .clone(),
    )
    .expect("handle_token is a string")
}

fn request_path(token: &str) -> String {
    format!("/org/freedesktop/portal/desktop/request/fake/{token}")
}

/// Completes a request the way the portal spec says: a `Response` signal on
/// the request handle, carrying the code and results dict.
async fn emit_response(
    conn: &zbus::Connection,
    request: &str,
    code: u32,
    results: HashMap<String, OwnedValue>,
) {
    let msg = Message::signal(request, REQUEST_IFACE, "Response")
        .expect("building the Response signal header")
        .build(&(code, results))
        .expect("building the Response signal body");
    conn.send(&msg).await.expect("sending the Response");
}

/// The test thread's end of the served portal: registration and raw signal
/// sends over the blocking wrapper.
struct PortalHandle {
    conn: zbus::blocking::Connection,
    bound_session: Arc<Mutex<Option<String>>>,
}

impl PortalHandle {
    /// Fires the `Activated` signal for the bound shortcut — the synthetic
    /// keypress of this suite.
    fn fire_activated(&self) {
        let msg = Message::signal(SESSION_PATH, GLOBALSHORTCUTS_IFACE, "Activated")
            .expect("building the Activated header")
            .build(&(
                OwnedObjectPath::try_from(SESSION_PATH).expect("session path"),
                "hop-toggle".to_string(),
                0u64,
                HashMap::<String, OwnedValue>::new(),
            ))
            .expect("building the Activated body");
        self.conn.send(&msg).expect("sending Activated");
    }
}

// ---------------------------------------------------------------------------
// The fixture: bus, fake, config, PATH stub, stderr capture.
// ---------------------------------------------------------------------------

/// Which fake-portal behaviour a test wants.
enum Mode {
    /// Accept everything; the round-trip arm.
    Accept,
    /// Accept `CreateSession`, refuse `BindShortcuts`; criterion 3's
    /// bind-refusal arm.
    RefuseBind,
    /// Serve nothing at all — a bus with no portal owner; the
    /// portal-absent arm.
    Absent,
}

/// Everything one test needs, or `None` (with a printed reason) when the
/// environment cannot host it.
struct Fixture {
    _bus: SessionBus,
    // Held, not just its path: a dropped TempDir deletes the tree out from
    // under every process pointing at it.
    runtime: tempfile::TempDir,
    bus_address: String,
    portal: Option<PortalHandle>,
}

impl Fixture {
    fn start(mode: Mode) -> Option<Fixture> {
        if find_in_path("dbus-daemon").is_none() {
            eprintln!(
                "skipping: dbus-daemon not found on $PATH — install the `dbus` \
                 package (CI does)"
            );
            return None;
        }
        let bus = SessionBus::start().expect("spawning dbus-daemon");
        let runtime = tempfile::tempdir().expect("temporary runtime directory");

        // Config: the [hotkey] section main.rs reads.
        let config_dir = runtime.path().join("config").join("hop");
        fs::create_dir_all(&config_dir).expect("creating the config directory");
        fs::write(
            config_dir.join("config.toml"),
            format!("[hotkey]\ntoggle = \"{BINDING}\"\n"),
        )
        .expect("writing the config");

        // The recording `hop` stub: spawn_toggle runs `hop toggle` from
        // PATH; this one appends the marker line that proves the trip.
        let stub_dir = runtime.path().join("stub-bin");
        fs::create_dir_all(&stub_dir).expect("creating the stub directory");
        let stub = stub_dir.join("hop");
        fs::write(
            &stub,
            "#!/bin/sh\nprintf 'fired\\n' >> \"$HOP_TEST_MARKER\"\n",
        )
        .expect("writing the hop stub");
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755))
            .expect("making the hop stub executable");

        // The fake portal, served from this process on the private bus.
        // Built by explicit address — the test process itself must not grow
        // a DBUS_SESSION_BUS_ADDRESS that parallel tests would share.
        let portal = match mode {
            Mode::Absent => None,
            Mode::Accept | Mode::RefuseBind => {
                let conn = zbus::blocking::connection::Builder::address(bus.address.as_str())
                    .expect("parsing the private bus address")
                    // The whole point of the probe: a well-known name for
                    // `org.freedesktop.portal.Desktop` to be owned by.
                    .name(PORTAL_SERVICE)
                    .expect("requesting the portal well-known name")
                    .build()
                    .expect("connecting the fake portal to the private bus");
                let refuse_bind = Arc::new(AtomicBool::new(matches!(mode, Mode::RefuseBind)));
                let bound_session = Arc::new(Mutex::new(None));
                conn.object_server()
                    .at(
                        PORTAL_PATH,
                        FakePortal {
                            conn: conn.inner().clone(),
                            refuse_bind: Arc::clone(&refuse_bind),
                            bound_session: Arc::clone(&bound_session),
                        },
                    )
                    .expect("serving the fake GlobalShortcuts interface");
                Some(PortalHandle {
                    conn,
                    bound_session,
                })
            }
        };

        Some(Fixture {
            bus_address: bus.address.clone(),
            _bus: bus,
            runtime,
            portal,
        })
    }

    /// The `XDG_CONFIG_HOME` value: the config *root*, which `config.rs`
    /// extends with `hop/config.toml` itself.
    fn config_root(&self) -> PathBuf {
        self.runtime.path().join("config")
    }

    fn stub_dir(&self) -> PathBuf {
        self.runtime.path().join("stub-bin")
    }

    fn marker(&self) -> PathBuf {
        self.runtime.path().join("toggle-marker")
    }

    fn stderr_log(&self) -> PathBuf {
        self.runtime.path().join("daemon-stderr")
    }

    /// Spawns the real daemon against this fixture: private bus, private
    /// config, stubbed PATH, and **no DISPLAY** — so the X11 probe can only
    /// fail and the fallback arms land on guidance deterministically.
    fn spawn_daemon(&self) -> ChildProcess {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hop-hotkeyd"));
        command
            .env("DBUS_SESSION_BUS_ADDRESS", &self.bus_address)
            .env("XDG_CONFIG_HOME", self.config_root())
            .env("HOP_TEST_MARKER", self.marker())
            .env_remove("DISPLAY")
            .env(
                "PATH",
                std::env::join_paths(std::iter::once(self.stub_dir()).chain(
                    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
                ))
                .expect("joining PATH"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                fs::File::create(self.stderr_log()).expect("stderr file"),
            ));
        ChildProcess {
            child: command.spawn().expect("spawning hop-hotkeyd"),
        }
    }

    /// The daemon's stderr so far, as one string.
    fn stderr(&self) -> String {
        let mut log = fs::File::open(self.stderr_log()).expect("opening the stderr log");
        let mut text = String::new();
        let _ = log.read_to_string(&mut text);
        text
    }

    fn marker_lines(&self) -> usize {
        match fs::File::open(self.marker()) {
            Ok(mut file) => {
                let mut text = String::new();
                let _ = file.read_to_string(&mut text);
                text.lines().count()
            }
            Err(_) => 0,
        }
    }

    /// Dumps the daemon's startup stderr when `HOP_DUMP_STDERR` is set —
    /// the hook issue #235's verification used to quote the exact
    /// backend-selection lines for each outcome; harmless noise otherwise.
    fn dump_stderr(&self) {
        if std::env::var_os("HOP_DUMP_STDERR").is_some() {
            eprintln!(
                "--- hop-hotkeyd stderr ---\n{}\n--------------------------",
                self.stderr()
            );
        }
    }

    /// Waits for the daemon to exit on its own (the guidance arms do),
    /// asserting a clean zero status — never a crash, never silence.
    fn wait_for_clean_exit(&self, daemon: &mut ChildProcess) {
        let deadline = Instant::now() + POLL_TIMEOUT;
        loop {
            if let Some(status) = daemon.child.try_wait().expect("polling hop-hotkeyd") {
                assert!(
                    status.success(),
                    "guidance must exit 0 (criterion 3), got: {status}; \
                     stderr:\n{}",
                    self.stderr()
                );
                self.dump_stderr();
                return;
            }
            assert!(
                Instant::now() < deadline,
                "hop-hotkeyd neither exited nor chose a backend within {}s; \
                 stderr:\n{}",
                POLL_TIMEOUT.as_secs(),
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

// ---------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------

/// Criterion 1: the full portal round trip against the real binary —
/// probe, CreateSession/BindShortcuts accepted ("chosen backend" logged),
/// synthetic `Activated`, marker written by the stubbed `hop toggle`,
/// daemon still resident afterward.
#[test]
fn portal_round_trip_activates_the_toggle() {
    let Some(fixture) = Fixture::start(Mode::Accept) else {
        return; // skipped with a printed reason
    };
    let portal = fixture
        .portal
        .as_ref()
        .expect("Accept mode serves a portal");
    let mut daemon = fixture.spawn_daemon();

    poll_until("startup backend-selection log", || {
        fixture
            .stderr()
            .contains("backend portal chosen")
            .then_some(())
    });
    poll_until("the fake portal saw BindShortcuts accepted", || {
        portal
            .bound_session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
            .then_some(())
    });

    portal.fire_activated();

    poll_until(
        &format!(
            "`hop toggle` ran and wrote its marker; stderr:\n{}",
            fixture.stderr()
        ),
        || (fixture.marker_lines() >= 1).then_some(()),
    );
    // Criterion 1's residency half: still alive holding the session after
    // dispatching, exactly like the X11 grab loop's contract.
    assert!(
        daemon
            .child
            .try_wait()
            .expect("polling hop-hotkeyd")
            .is_none(),
        "the daemon must stay resident after an activation"
    );
    fixture.dump_stderr();
}

/// Criterion 3, absent arm: a reachable bus with no portal owner degrades
/// to the next backend (X11 probed, fails headless), then to printed
/// guidance and exit 0 — logged at every step, never silent, never a crash.
#[test]
fn absent_portal_falls_through_to_x11_then_guidance() {
    let Some(fixture) = Fixture::start(Mode::Absent) else {
        return;
    };
    let mut daemon = fixture.spawn_daemon();
    fixture.wait_for_clean_exit(&mut daemon);

    let stderr = fixture.stderr();
    for expected in [
        // The fall-through names why the portal was skipped…
        "backend portal unavailable",
        "no service owns org.freedesktop.portal.Desktop",
        "falling back to the X11 grab",
        // …why X11 could not take over either…
        "backend X11 grab unavailable",
        // …and the guidance arm prints the DE one-liners (criterion 4).
        "no automatic backend applies",
        "hop toggle",
    ] {
        assert!(
            stderr.contains(expected),
            "missing `{expected}` in:\n{stderr}"
        );
    }
}

/// Criterion 3, refused arm: a portal that accepts `CreateSession` but
/// refuses `BindShortcuts` produces the distinct bind-refusal reason and
/// the same clean degrade to guidance.
#[test]
fn bind_refusal_degrades_to_the_next_backend() {
    let Some(fixture) = Fixture::start(Mode::RefuseBind) else {
        return;
    };
    let mut daemon = fixture.spawn_daemon();
    fixture.wait_for_clean_exit(&mut daemon);

    let stderr = fixture.stderr();
    for expected in [
        "backend portal bind refused",
        "BindShortcuts refused (response code 1)",
        "falling back to the X11 grab",
        "no automatic backend applies",
        "hop toggle",
    ] {
        assert!(
            stderr.contains(expected),
            "missing `{expected}` in:\n{stderr}"
        );
    }
    // The wording split the units pin: a refusal is reported distinctly
    // from an absent portal.
    assert!(
        !fixture.stderr().contains("backend portal unavailable"),
        "a refused bind must not be misreported as an absent portal:\n{stderr}"
    );
}
