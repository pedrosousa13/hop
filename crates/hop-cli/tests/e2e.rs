//! End-to-end tests that drive the real `hop` binary against a real `hopd`
//! process over a real Unix socket. Nothing here mocks the daemon or the
//! transport: `the_cli_query_round_trips_and_exits_zero` spawns
//! `env!("CARGO_BIN_EXE_hop")` as a subprocess and inspects the exit code
//! and stdout it produces, the same way a shell script driving both
//! binaries would.
#![allow(clippy::unwrap_used)]

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use hop_protocol::framing::{FRAME_PREFIX_LEN, decode_payload, encode_frame, payload_len};
use hop_protocol::{API_VERSION, ClientMsg, DaemonMsg, ExecOutcome, Item};

/// `CARGO_BIN_EXE_hopd` is not set here: Cargo only defines a
/// `CARGO_BIN_EXE_<bin>` variable for binaries the *current* package builds,
/// and `hopd` belongs to the `hopd` crate, not `hop-cli`. What Cargo does
/// give this test is `CARGO_BIN_EXE_hop`, and under any build that also
/// built `hopd` the two binaries land as siblings in the same output
/// directory — which is the case `cargo test --workspace` guarantees, since
/// it builds every workspace member's binaries before running any test.
/// `cargo test -p hop-cli` alone does not build `hopd` at all, so the
/// `assert!` below turns that corner into a named failure instead of a
/// confusing "No such file or directory" from `Command::spawn`.
fn hopd_binary_path() -> PathBuf {
    let hop_path = Path::new(env!("CARGO_BIN_EXE_hop"));
    let hopd_path = hop_path.parent().unwrap().join("hopd");
    assert!(
        hopd_path.exists(),
        "hopd binary not built — run cargo test --workspace"
    );
    hopd_path
}

/// A spawned `hopd`, killed on drop so a failing assertion never leaks a
/// daemon process into the rest of the suite.
///
/// This mirrors `crates/hopd/tests/socket.rs`'s `DaemonProcess` /
/// `spawn_daemon` almost exactly. They are duplicated rather than shared: a
/// test-only helper this small (~20 lines) does not carry its weight as a
/// third crate, and the two copies drift only if the daemon's startup
/// contract changes, which is exactly when a diff in both files is the
/// useful signal.
struct DaemonProcess {
    child: Child,
    socket_path: PathBuf,
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawns `hopd` at `hopd_path` with `XDG_RUNTIME_DIR` set to `runtime_dir`,
/// and polls for its socket to appear before handing the process back. Same
/// 100ms/50-attempt (5s total) shape as `hopd/tests/socket.rs`'s helper, for
/// the same reason: fast on the common case, bounded on a stuck one.
///
/// `HOME`, `XDG_DATA_HOME` and `XDG_DATA_DIRS` are pinned to paths under
/// `runtime_dir` that this test never populates, mirroring
/// `hopd/tests/socket.rs`'s `spawn_daemon` and for the identical reason: the
/// daemon this spawns registers a real, environment-backed apps provider
/// (`hopd::apps::build_apps_provider`) alongside the skeleton one, and that
/// provider answers from whatever `.desktop` files actually exist under
/// those roots. Left unset, `the_cli_query_round_trips_and_exits_zero`'s
/// query would run against the *host machine's* real applications rather
/// than an empty index, and its `lines.len() == 1` assertion would fail on
/// any machine with an installed application whose title, generic name,
/// comment, keywords or exec command happens to contain that query's term.
/// This closes the two roots `build_apps_provider` reads from `std::env`; it
/// cannot close the hardcoded, unparameterized Flatpak system export
/// directory `flatpak_application_roots` always includes
/// (`/var/lib/flatpak/exports/share/applications`), which remains a
/// residual, unclosable risk — see `hopd/tests/socket.rs`'s `spawn_daemon`
/// doc comment for the fuller account of why that root can't be closed here.
fn spawn_daemon(hopd_path: &Path, runtime_dir: &Path) -> DaemonProcess {
    // `state_dir::resolve` treats a missing parent *base* directory as an
    // error — it creates only the `hop` dir inside it, not recursively — so
    // the isolated state-home root must exist first, mirroring
    // `hopd/tests/socket.rs`'s `spawn_daemon`. `config::load` has no such
    // requirement (a missing leaf file or a missing ancestor directory both
    // map to `Ok(Config::default())`); the config root is pre-created anyway
    // to keep the two isolated roots symmetric.
    std::fs::create_dir_all(runtime_dir.join("isolated-xdg-state-home")).unwrap();
    std::fs::create_dir_all(runtime_dir.join("isolated-xdg-config-home")).unwrap();

    let child = Command::new(hopd_path)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("HOME", runtime_dir.join("isolated-home"))
        .env("XDG_DATA_HOME", runtime_dir.join("isolated-xdg-data-home"))
        .env("XDG_DATA_DIRS", "")
        .env(
            "XDG_CONFIG_HOME",
            runtime_dir.join("isolated-xdg-config-home"),
        )
        .env(
            "XDG_STATE_HOME",
            runtime_dir.join("isolated-xdg-state-home"),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn hopd");

    let socket_path = runtime_dir.join("hop").join("hopd.sock");
    let process = DaemonProcess { child, socket_path };

    for _ in 0..50 {
        if process.socket_path.exists() {
            return process;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    panic!(
        "hopd socket did not appear at {:?} within 5s",
        process.socket_path
    );
}

/// Like [`spawn_daemon`], but starts `hopd` with `--socket <path>` rather
/// than letting it derive the default — issue #180's override, exercised
/// here from the `hop` side of the wire rather than `hopd`'s own
/// `crates/hopd/tests/socket.rs` (which already proves the daemon binds an
/// override correctly and narrows it to 0700/0600). `socket_path` deliberately
/// sits one level below `runtime_dir` at a name other than `hop`
/// (`hop-dev`), the exact non-conflicting-dev-socket case design decision D2
/// of this issue's plan is written around.
fn spawn_daemon_with_socket(
    hopd_path: &Path,
    runtime_dir: &Path,
    socket_path: &Path,
) -> DaemonProcess {
    std::fs::create_dir_all(runtime_dir.join("isolated-xdg-state-home")).unwrap();
    std::fs::create_dir_all(runtime_dir.join("isolated-xdg-config-home")).unwrap();

    let child = Command::new(hopd_path)
        .arg("--socket")
        .arg(socket_path)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("HOME", runtime_dir.join("isolated-home"))
        .env("XDG_DATA_HOME", runtime_dir.join("isolated-xdg-data-home"))
        .env("XDG_DATA_DIRS", "")
        .env(
            "XDG_CONFIG_HOME",
            runtime_dir.join("isolated-xdg-config-home"),
        )
        .env(
            "XDG_STATE_HOME",
            runtime_dir.join("isolated-xdg-state-home"),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn hopd with --socket");

    let process = DaemonProcess {
        child,
        socket_path: socket_path.to_path_buf(),
    };

    for _ in 0..50 {
        if process.socket_path.exists() {
            return process;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    panic!(
        "hopd socket did not appear at {:?} within 5s",
        process.socket_path
    );
}

/// Issue #180 criterion 2: `hop --socket <path>` connects to a daemon bound
/// at that overridden path rather than the derived one. The flag goes
/// *before* the subcommand (design decision D7) — `hop --socket <path>
/// query <text>` — since anything after `query` becomes query text instead.
#[test]
fn the_cli_socket_flag_reaches_a_daemon_bound_there() {
    let hopd_path = hopd_binary_path();
    let runtime_dir = tempfile::tempdir().unwrap();
    let socket_path = runtime_dir.path().join("hop-dev").join("hopd.sock");
    let _daemon = spawn_daemon_with_socket(&hopd_path, runtime_dir.path(), &socket_path);

    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("--socket")
        .arg(&socket_path)
        .arg("query")
        .arg("walking skeleton")
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .output()
        .expect("failed to run hop --socket query");

    assert!(
        output.status.success(),
        "hop --socket <path> query must exit 0 against a daemon bound there, got {:?}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout must be valid UTF-8");
    let items: Vec<Item> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line must parse as an Item"))
        .collect();
    assert!(
        items
            .iter()
            .any(|item| item.title.as_str() == "Hello from hopd"),
        "expected the skeleton item among the results, got {stdout:?}"
    );

    // The default path was never touched by either binary: proof this ran
    // against the override, not a daemon that happened to also be listening
    // at the derived location.
    assert!(
        !runtime_dir.path().join("hop").join("hopd.sock").exists(),
        "the default socket path must never come into existence for an override-only run"
    );
}

/// Issue #180 criterion 3, from `hop`'s side: an override that resolves
/// outside `$XDG_RUNTIME_DIR` is refused, naming the rule, and never falls
/// back to the derived path. No daemon is spawned for this test — the
/// refusal must happen before `hop` ever tries to connect.
#[test]
fn the_cli_socket_flag_outside_the_runtime_dir_is_refused() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let outside_path = elsewhere.path().join("hopd.sock");

    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("--socket")
        .arg(&outside_path)
        .arg("query")
        .arg("anything")
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .output()
        .expect("failed to run hop --socket");

    assert!(
        !output.status.success(),
        "an out-of-runtime-dir override must be refused"
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "the refusal must exit with the same code Command::Usage does"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("XDG_RUNTIME_DIR"),
        "stderr must name the rule that was broken, got: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "a refused override must print nothing to stdout"
    );
    assert!(
        !outside_path.exists(),
        "a refused override must never cause a connection attempt that creates anything at the path"
    );
}

#[test]
fn the_cli_query_round_trips_and_exits_zero() {
    let hopd_path = hopd_binary_path();
    let runtime_dir = tempfile::tempdir().unwrap();
    let _daemon = spawn_daemon(&hopd_path, runtime_dir.path());

    // `SkeletonProvider::query` ignores its query text and always answers
    // with the same hardcoded item, but the daemon now runs that item through
    // `Pipeline::assemble` (issue #103) before it reaches a client, and
    // `Ranker::rank` drops anything whose haystack does not fuzzy-match the
    // term — so a nonsense token that used to prove the round trip by
    // returning the one hardcoded item now correctly returns nothing.
    // "walking skeleton" is chosen instead because it matches the skeleton
    // item's haystack (`Hello from hopd` + `M2.2 walking skeleton`) on both
    // atoms, while an installed application whose title and subtitle contain
    // both "walking" and "skeleton" is implausible — but not impossible, so
    // the assertion below checks the output *contains* the item rather than
    // assuming it is the only one. That is also why this test does not assert
    // an exact line count: doing so would depend on what happens to be
    // installed on whatever machine runs it, which is the one root
    // `spawn_daemon` cannot close (its own doc comment names it: the
    // hardcoded, unparameterized Flatpak system export directory).
    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("query")
        .arg("walking skeleton")
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .output()
        .expect("failed to run hop query");

    assert!(
        output.status.success(),
        "hop query must exit 0, got {:?}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout must be valid UTF-8");
    let items: Vec<Item> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line must parse as an Item"))
        .collect();
    assert!(
        items
            .iter()
            .any(|item| item.title.as_str() == "Hello from hopd"),
        "expected the skeleton item among the results, got {stdout:?}"
    );
}

#[test]
fn the_cli_exec_sends_execute_against_the_live_result_and_exits_zero() {
    // The exec flow (issue #59): `hop exec <query> <item-id> <action-id>`
    // queries, then — only after `QueryDone` — sends an `Execute` frame
    // naming the item from the last results frame, and maps an `Executed`
    // reply to exit 0. The fake daemon answers the query with one item and
    // then reads the `Execute` frame the CLI sends, proving the resolution
    // reached the wire with the right ids.
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = fake_daemon(runtime_dir.path(), |stream, id| {
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id,
                partial: true,
                items: vec![tiny_item(1, "an app")],
            },
        );
        write_daemon_frame(stream, &DaemonMsg::QueryDone { query_id: id });

        let ClientMsg::Execute {
            query_id,
            item_id,
            action_id,
        } = read_client_frame(stream)
        else {
            panic!("expected an Execute frame after QueryDone");
        };
        assert_eq!(query_id, id, "execute must name the query id");
        assert_eq!(
            item_id.as_str(),
            "test:1",
            "execute must name the delivered item"
        );
        assert_eq!(
            action_id.as_str(),
            "open",
            "execute must name the resolved action"
        );

        write_daemon_frame(
            stream,
            &DaemonMsg::Executed {
                query_id: id,
                outcome: ExecOutcome::Done,
            },
        );
    });

    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("exec")
        .arg("an app")
        .arg("test:1")
        .arg("open")
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .output()
        .unwrap();
    daemon.join().unwrap();

    assert!(
        output.status.success(),
        "hop exec must exit 0 on an Executed reply, got {:?}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Runs `hop exec an app test:1 open` against a fresh fake daemon that
/// answers the query with one item (`test:1`, action `open`) and, reading the
/// `Execute` frame the CLI sends, replies with `make_reply(id)`. Returns the
/// subprocess's output so a test can assert its real exit code.
fn run_exec_against_reply(
    make_reply: impl Fn(u64) -> DaemonMsg + Send + 'static,
) -> std::process::Output {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = fake_daemon(runtime_dir.path(), move |stream, id| {
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id,
                partial: true,
                items: vec![tiny_item(1, "an app")],
            },
        );
        write_daemon_frame(stream, &DaemonMsg::QueryDone { query_id: id });
        // The exec flow resolves the item+action locally (it is in the frame
        // above), sends Execute, then reads the daemon's reply.
        let ClientMsg::Execute { .. } = read_client_frame(stream) else {
            panic!("expected an Execute frame after QueryDone");
        };
        write_daemon_frame(stream, &make_reply(id));
    });

    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("exec")
        .arg("an app")
        .arg("test:1")
        .arg("open")
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .output()
        .unwrap();
    daemon.join().unwrap();
    output
}

fn query_scoped_error(id: u64, code: hop_protocol::ErrorCode, message: &'static str) -> DaemonMsg {
    DaemonMsg::Error {
        query_id: Some(id),
        error: hop_protocol::ProtoError::new(code, hop_protocol::ErrorDetail::Fixed(message)),
    }
}

/// Criterion 6: the process's real exit code distinguishes success, unknown
/// item, unknown action, and provider failure. Each case drives the binary and
/// reads back its actual `ExitCode` — a regression that mapped a refusal back
/// to generic failure (1) would fail here.
#[test]
fn the_cli_exec_exit_codes_distinguish_each_outcome() {
    // Success → 0.
    let output = run_exec_against_reply(|id| DaemonMsg::Executed {
        query_id: id,
        outcome: ExecOutcome::Done,
    });
    assert_eq!(
        output.status.code(),
        Some(0),
        "success must exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Unknown item → 10.
    let output = run_exec_against_reply(|id| {
        query_scoped_error(id, hop_protocol::ErrorCode::UnknownItem, "nope")
    });
    assert_eq!(
        output.status.code(),
        Some(10),
        "an unknown-item refusal must exit 10, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Unknown action → 11.
    let output = run_exec_against_reply(|id| {
        query_scoped_error(id, hop_protocol::ErrorCode::UnknownAction, "nope")
    });
    assert_eq!(
        output.status.code(),
        Some(11),
        "an unknown-action refusal must exit 11, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Provider failed → 12.
    let output = run_exec_against_reply(|id| {
        query_scoped_error(id, hop_protocol::ErrorCode::ProviderFailed, "boom")
    });
    assert_eq!(
        output.status.code(),
        Some(12),
        "a provider failure must exit 12, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_version_subcommand_prints_both_versions() {
    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("version")
        .output()
        .expect("failed to run hop version");

    assert!(
        output.status.success(),
        "hop version must exit 0, got {:?}",
        output.status
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout must be valid UTF-8");
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "stdout must contain the CLI's own version, got: {stdout:?}"
    );
    // Derived from `API_VERSION` rather than written as a literal. The point
    // of this assertion is that `hop version` reports the protocol version it
    // actually speaks — a hardcoded number tests that the constant has one
    // particular value, which is a different and much less useful claim, and
    // it is what made this test fail on the #127 bump rather than catching
    // anything.
    let expected = format!("protocol {}", hop_protocol::API_VERSION);
    assert!(
        stdout.contains(&expected),
        "stdout must contain {expected:?}, got: {stdout:?}"
    );
}

/// Issue #180's own acceptance criterion 6: omitting `--socket` is
/// unchanged behavior. `hop version` never opens a socket at all — it
/// prints only compile-time constants — so it must keep working even in an
/// environment with no `$XDG_RUNTIME_DIR`, exactly as it did before this
/// issue. This is the regression the coordinator's review caught: an
/// earlier version of `main.rs` resolved the socket path unconditionally
/// before dispatching on the parsed command, which made `hop version`
/// depend on a resolvable runtime directory it never uses.
#[test]
fn the_version_subcommand_works_with_no_xdg_runtime_dir_set() {
    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("version")
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .expect("failed to run hop version");

    assert!(
        output.status.success(),
        "hop version must succeed with no XDG_RUNTIME_DIR, got {:?}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout must be valid UTF-8");
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "stdout must still contain the CLI's own version, got: {stdout:?}"
    );
}

/// A scripted daemon: binds the socket where `hop` will look, accepts one
/// connection, answers the handshake, hands the accepted stream to `script`,
/// and keeps listening so the CLI's whole exchange happens against bytes
/// this test chose. Runs on a thread; joined via the returned handle so a
/// panic inside the script fails the test instead of vanishing.
fn fake_daemon(
    runtime_dir: &Path,
    script: impl FnOnce(&mut std::os::unix::net::UnixStream, u64) + Send + 'static,
) -> std::thread::JoinHandle<()> {
    let hop_dir = runtime_dir.join("hop");
    std::fs::create_dir_all(&hop_dir).unwrap();
    let listener = UnixListener::bind(hop_dir.join("hopd.sock")).unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        // Handshake: expect Hello, answer HelloAck.
        let hello = read_client_frame(&mut stream);
        assert!(matches!(hello, ClientMsg::Hello { .. }));
        write_daemon_frame(
            &mut stream,
            &DaemonMsg::HelloAck {
                api_version: API_VERSION,
            },
        );
        // Expect the query; its id is what the script frames must reference.
        let ClientMsg::Query { id, .. } = read_client_frame(&mut stream) else {
            panic!("expected the query frame after the handshake");
        };
        script(&mut stream, id);
    })
}

fn read_client_frame(stream: &mut std::os::unix::net::UnixStream) -> ClientMsg {
    let mut prefix = [0u8; FRAME_PREFIX_LEN];
    stream.read_exact(&mut prefix).unwrap();
    let len = payload_len(prefix).unwrap();
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).unwrap();
    decode_payload(&payload).unwrap()
}

fn write_daemon_frame(stream: &mut std::os::unix::net::UnixStream, msg: &DaemonMsg) {
    stream.write_all(&encode_frame(msg).unwrap()).unwrap();
}

fn tiny_item(n: usize, title: &str) -> Item {
    use hop_protocol::{Action, ActionId, ActionKind, ItemId, Kind};
    Item {
        id: ItemId::new(format!("test:{n}")).unwrap(),
        kind: Kind::Action,
        title: hop_protocol::ItemTitle::new(title).unwrap(),
        subtitle: None,
        icon: None,
        actions: vec![Action {
            id: ActionId::new("open").unwrap(),
            kind: ActionKind::Open,
            label: "Open".to_string(),
        }],
        default_action: ActionId::new("open").unwrap(),
        copy_text: None,
        append_to_end: false,
        provider: "test".to_string(),
    }
}

#[test]
fn the_cli_drops_frames_whose_query_id_is_not_current() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = fake_daemon(runtime_dir.path(), |stream, id| {
        // A stale frame (wrong id) before, between, and after the real ones:
        // none of the "stale" titles may reach stdout. The real ones grow
        // cumulatively frame over frame — each carries the complete current
        // list per `DaemonMsg::Results`' contract, not just what is new —
        // since that is what a conforming daemon sends and this test's job
        // is to prove id-filtering, not replacement itself (that is
        // `the_cli_prints_only_the_last_frames_items` below).
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id + 1,
                partial: true,
                items: vec![tiny_item(1, "stale before")],
            },
        );
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id,
                partial: true,
                items: vec![tiny_item(2, "current one")],
            },
        );
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id + 1,
                partial: true,
                items: vec![tiny_item(3, "stale between")],
            },
        );
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id,
                partial: true,
                items: vec![tiny_item(2, "current one"), tiny_item(4, "current two")],
            },
        );
        write_daemon_frame(stream, &DaemonMsg::QueryDone { query_id: id + 1 }); // stale done: must NOT end the query
        // Proof that the stale `QueryDone` above did not end the exchange:
        // this frame is sent *after* it, naming the real query id. A loop
        // that (incorrectly) ends on any `QueryDone` regardless of id would
        // never read this frame, so "current three" would be missing from
        // stdout — that is the failure this frame exists to catch.
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id,
                partial: true,
                items: vec![
                    tiny_item(2, "current one"),
                    tiny_item(4, "current two"),
                    tiny_item(5, "current three"),
                ],
            },
        );
        write_daemon_frame(stream, &DaemonMsg::QueryDone { query_id: id });
    });

    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("query")
        .arg("q")
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .output()
        .unwrap();
    daemon.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("current one")
            && stdout.contains("current two")
            && stdout.contains("current three")
    );
    assert!(
        !stdout.contains("stale"),
        "a stale frame's items must never be rendered, got: {stdout}"
    );
    // Assembled output: the three current items, in delivery order. This
    // also proves "current three" (sent after the stale `QueryDone`) made
    // it into stdout, so the exchange survived that stale done.
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3);
    let one = stdout.find("current one").expect("current one must print");
    let two = stdout.find("current two").expect("current two must print");
    let three = stdout
        .find("current three")
        .expect("current three must print");
    assert!(
        one < two && two < three,
        "items must print in delivery order, got: {stdout}"
    );
}

#[test]
fn the_cli_drops_an_error_frame_scoped_to_another_query() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = fake_daemon(runtime_dir.path(), |stream, id| {
        // An error naming a query this process is not waiting on. Per
        // `DaemonMsg::Error`'s contract a `Some(id)` error is terminal for
        // that exchange alone, so this must be dropped exactly like a stale
        // `results` frame — not treated as fatal. The frames after it prove
        // the exchange survived.
        write_daemon_frame(
            stream,
            &DaemonMsg::Error {
                query_id: Some(id + 1),
                error: hop_protocol::ProtoError::new(
                    hop_protocol::ErrorCode::UnknownItem,
                    hop_protocol::ErrorDetail::Fixed("stale query's problem, not this one's"),
                ),
            },
        );
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id,
                partial: true,
                items: vec![tiny_item(1, "survived the stale error")],
            },
        );
        write_daemon_frame(stream, &DaemonMsg::QueryDone { query_id: id });
    });

    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("query")
        .arg("q")
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .output()
        .unwrap();
    daemon.join().unwrap();

    assert!(
        output.status.success(),
        "an error scoped to another query must not kill this one, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("survived the stale error"),
        "the current query's item must still print, got: {stdout}"
    );
}

#[test]
fn the_cli_fails_on_an_error_frame_scoped_to_its_own_query() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = fake_daemon(runtime_dir.path(), |stream, id| {
        // The other half of the contract: an error naming *this* exchange is
        // terminal for it, and no `QueryDone` follows. Sent after a results
        // frame, so this also pins that nothing already assembled is printed
        // for an exchange that ended badly.
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id,
                partial: true,
                items: vec![tiny_item(1, "assembled but never shown")],
            },
        );
        write_daemon_frame(
            stream,
            &DaemonMsg::Error {
                query_id: Some(id),
                error: hop_protocol::ProtoError::new(
                    hop_protocol::ErrorCode::ProviderFailed,
                    hop_protocol::ErrorDetail::Fixed("this exchange is over"),
                ),
            },
        );
    });

    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("query")
        .arg("q")
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .output()
        .unwrap();
    daemon.join().unwrap();

    assert!(
        !output.status.success(),
        "an error naming this query must end it as a failure"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("this exchange is over"),
        "the daemon's message must reach stderr, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "nothing may be printed for an exchange that ended in an error"
    );
}

#[test]
fn the_cli_prints_only_the_last_frames_items_not_every_frame_ever_seen() {
    // The replace rule itself (`DaemonMsg::Results`' doc comment, issue
    // #103): each frame is the complete current list, and a client swaps its
    // held list for it rather than extending. This daemon sends a first
    // frame, then a second whose items neither superset nor overlap the
    // first's — under the old append behavior both frames' items would print
    // (three lines, "old only" among them); under replace only the second
    // frame's two items survive.
    let runtime_dir = tempfile::tempdir().unwrap();
    let daemon = fake_daemon(runtime_dir.path(), |stream, id| {
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id,
                partial: true,
                items: vec![tiny_item(1, "old only")],
            },
        );
        write_daemon_frame(
            stream,
            &DaemonMsg::Results {
                query_id: id,
                partial: true,
                items: vec![tiny_item(2, "new one"), tiny_item(3, "new two")],
            },
        );
        write_daemon_frame(stream, &DaemonMsg::QueryDone { query_id: id });
    });

    let output = Command::new(env!("CARGO_BIN_EXE_hop"))
        .arg("query")
        .arg("q")
        .env("XDG_RUNTIME_DIR", runtime_dir.path())
        .output()
        .unwrap();
    daemon.join().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("old only"),
        "the first frame's item must not survive the second frame's replacement, got: {stdout}"
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "expected exactly the last frame's two items, got: {stdout:?}"
    );
    assert!(stdout.contains("new one") && stdout.contains("new two"));
}
