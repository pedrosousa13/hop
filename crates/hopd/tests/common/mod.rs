//! Client-side helpers shared by this crate's integration tests: framing one
//! message, reading one frame, and the handshake preamble. Kept as a `common`
//! module rather than duplicated per test file so a wire-contract change
//! shows up as one diff here, not a drift between suites.
//!
//! This module also holds the in-process daemon harness (`TestDaemon`,
//! `start_daemon`) and the scripted-provider fixture (`Script`,
//! `ScriptedProvider`, `RecordingLog`, `scripted_item`). Both live here
//! rather than in the test files that use them so `lifecycle.rs` and
//! `host.rs` share one harness instead of duplicating it.
//!
//! It also holds `checked_items`, the fixture that turns a bare `Vec<Item>`
//! into a genuine [`CheckedItems`] via a real [`Provider`] and
//! [`CheckedItems::check`] — what a [`ResultSource`] hands the daemon since
//! issue #85 made the per-item field-bound check a compiler-enforced
//! contract rather than a property `HostSource` merely happened to have.
//! `exec.rs` and `lifecycle.rs` each grew their own copy of this and its
//! backing `FixtureProvider` when #85 landed; they are one helper here
//! instead, because two copies differing only in a provider id string is
//! exactly the drift this module's fixtures exist to prevent. It always
//! chunks its input at [`MAX_ITEMS_PER_PROVIDER_ANSWER`] before calling
//! `check`, rather than building one `ProviderOutput` for the whole list:
//! for a fixture short enough to need no chunking that is one chunk and
//! behaves exactly as an unchunked call would, and for `lifecycle.rs`'s
//! over-the-frame-bound scenarios it is what lets a batch longer than the
//! per-provider cap reach the connection intact instead of being truncated
//! by `check` itself — see `a_list_over_the_frame_bound_is_truncated_and_terminates`
//! in `lifecycle.rs` for the test that needs that.
//!
//! `mod common;` compiles this whole module into each of the three test
//! binaries in this crate (`lifecycle`, `socket`, `host`), and each binary
//! uses only a subset of what is here: `socket.rs` drives a spawned `hopd`
//! binary and never calls `start_daemon` or touches the provider fixture;
//! `lifecycle.rs` uses the daemon harness but not the `ScriptedProvider`
//! fixture. Every item below is genuinely used — by a sibling binary, if not
//! by all three — so `dead_code` warnings here are false positives from
//! per-binary compilation, not real dead code. Allowed at the module level,
//! once, rather than scattered per item; no other module in this workspace
//! carries this allow.
#![allow(clippy::unwrap_used)]
#![allow(dead_code)]

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hop_core::host::{ProviderEvent, ProviderLog};
use hop_core::pipeline::{CheckedItems, MAX_ITEMS_PER_PROVIDER_ANSWER, ProviderOutput};
use hop_core::provider::{Provider, ProviderError, ProviderManifest, QueryCtx};
use hop_core::router::{Mode, RoutedQuery};
use hop_protocol::framing::{FRAME_PREFIX_LEN, decode_payload, encode_frame, payload_len};
use hop_protocol::{
    API_VERSION, ActionId, ClientMsg, DaemonMsg, ExecOutcome, Item, ItemId, ItemTitle, Kind,
};
use hopd::server::serve_with;
use hopd::source::ResultSource;

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

/// An in-process daemon on a scripted source, plus the runtime that hosts
/// it. Dropping this drops the runtime, which tears the server task and its
/// socket down with it.
pub struct TestDaemon {
    _runtime: tokio::runtime::Runtime,
    pub socket_path: PathBuf,
    _dir: tempfile::TempDir,
}

pub fn start_daemon<S: ResultSource>(source: S) -> TestDaemon {
    let dir = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let root = dir.path().to_path_buf();
    // serve_with expects the socket path itself, not the runtime directory
    // (hopd's runtime_dir::resolve, and the parent-creation issue #180 gave
    // the `--socket` override, are both binary-startup concerns, not
    // serve_with's own — see that function's doc comment, design decision
    // D4); create the 0700 parent the way either startup path would.
    let runtime_dir = root.join("hop");
    std::fs::create_dir(&runtime_dir).unwrap();
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket_path = runtime_dir.join(hop_protocol::socket::SOCKET_FILE_NAME);
    let serve_path = socket_path.clone();
    runtime.spawn(async move {
        // `false`: this harness never drives the `--socket` override, only
        // the ordinary derived-path startup every other test here exercises
        // — so there is nothing for the D5 inherited-listener warning to
        // ever fire about.
        let _ = serve_with(&serve_path, false, source).await;
    });

    for _ in 0..50 {
        if socket_path.exists() {
            return TestDaemon {
                _runtime: runtime,
                socket_path,
                _dir: dir,
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("in-process hopd socket did not appear at {socket_path:?} within 5s");
}

/// What a scripted provider does when it is asked to run — the fixture spec
/// §11 asks for, so an integration test's outcome is a property of the script
/// rather than of timing.
///
/// It lives here rather than in `hop-core` because only `hopd`'s integration
/// tests need it: exporting it from the library crate would mean a `testing`
/// feature or a permanently-compiled module, for a type no production caller
/// has any use for. Issues #57, #58 and #61 reuse it from here.
#[derive(Clone)]
pub enum Script {
    /// Answer with these items.
    Answer(Vec<Item>),
    /// Fail with this text — used for the bounding-and-stripping tests, so
    /// pass whatever hostile string is under test.
    Fail(String),
    /// Panic, to prove the host contains it.
    Panic,
    /// Never return, to prove the host cuts it off without cooperation. Yields
    /// while it waits, so `abort` can take effect and no worker thread is
    /// pinned for the test run.
    Hang,
}

/// A provider that does exactly what its [`Script`] says, and declares exactly
/// the manifest it was built with.
pub struct ScriptedProvider {
    manifest: ProviderManifest,
    script: Script,
}

impl ScriptedProvider {
    /// A provider answering to `id`, declaring `kinds`, serving `Mode::All`
    /// with no minimum term length, and running `script`.
    ///
    /// `budget` is 20 ms: comfortably above what an `Answer` or a `Fail` needs,
    /// and comfortably below the wait an integration test would notice, so a
    /// `Hang` resolves fast.
    pub fn new(id: &'static str, kinds: Vec<Kind>, script: Script) -> Self {
        ScriptedProvider {
            manifest: ProviderManifest {
                id,
                kinds,
                modes: vec![Mode::All],
                min_term_len: 0,
                budget: Duration::from_millis(20),
                ids_are_safe_to_persist_in_the_clear: false,
            },
            script,
        }
    }

    /// The same provider with a manifest field overridden — for the tests that
    /// need a specific budget, mode set or minimum.
    pub fn with_manifest(mut self, manifest: ProviderManifest) -> Self {
        self.manifest = manifest;
        self
    }
}

impl Provider for ScriptedProvider {
    fn manifest(&self) -> ProviderManifest {
        self.manifest.clone()
    }

    async fn query(
        self: Arc<Self>,
        _q: Arc<RoutedQuery>,
        _ctx: QueryCtx,
    ) -> Result<Vec<Item>, ProviderError> {
        match &self.script {
            Script::Answer(items) => Ok(items.clone()),
            Script::Fail(text) => Err(ProviderError::Failed(text.clone())),
            Script::Panic => panic!("scripted provider panic"),
            Script::Hang => loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
            },
        }
    }

    async fn execute(
        self: Arc<Self>,
        _item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        Ok(ExecOutcome::Done)
    }
}

/// A [`ProviderLog`] the tests can read back, so "a record was emitted" is an
/// assertion rather than an inspection of stderr.
#[derive(Default)]
pub struct RecordingLog {
    lines: Mutex<Vec<String>>,
}

impl RecordingLog {
    /// Every line recorded so far, in order.
    pub fn lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .expect("no test panics holding this")
            .clone()
    }
}

impl ProviderLog for RecordingLog {
    fn record(&self, event: ProviderEvent<'_>) {
        let line = match event {
            ProviderEvent::Answered {
                provider, items, ..
            } => {
                format!("answered {provider} {items}")
            }
            ProviderEvent::Failed(failure) => format!(
                "failed {} {:?} {}",
                failure.provider(),
                failure.kind(),
                failure.message()
            ),
            ProviderEvent::BudgetMiss { provider, .. } => format!("budget-miss {provider}"),
            ProviderEvent::Rejected {
                provider,
                rejections,
            } => {
                format!("rejected {provider} {}", rejections.len())
            }
            ProviderEvent::Skipped { provider } => format!("skipped {provider}"),
        };
        self.lines
            .lock()
            .expect("no test panics holding this")
            .push(line);
    }
}

/// One item, well-formed and agreeing with `provider` — the fixture's honest
/// item, for tests that need results rather than failures.
pub fn scripted_item(provider: &str, kind: Kind, id: &str, title: &str) -> Item {
    Item {
        id: ItemId::new(id).expect("within bounds by construction"),
        kind,
        title: ItemTitle::new(title).expect("within bounds by construction"),
        subtitle: None,
        icon: None,
        actions: vec![],
        default_action: ActionId::new("open").expect("within bounds by construction"),
        copy_text: None,
        append_to_end: false,
        provider: provider.to_string(),
    }
}

/// A provider that exists only to be a provider: [`CheckedItems::check`] can
/// be reached no other way, and [`checked_items`] needs a real one to build
/// a fixture batch as checked items (issue #85) rather than reaching into
/// `CheckedItems`'s private fields some other way. `query` and `execute` are
/// never called — every source built from [`checked_items`] scripts its own
/// batches and its own `execute` instead of actually running this provider.
struct FixtureProvider(ProviderManifest);

impl Provider for FixtureProvider {
    fn manifest(&self) -> ProviderManifest {
        self.0.clone()
    }

    async fn query(
        self: Arc<Self>,
        _q: Arc<RoutedQuery>,
        _ctx: QueryCtx,
    ) -> Result<Vec<Item>, ProviderError> {
        unreachable!("FixtureProvider::query is never called by these tests")
    }

    async fn execute(
        self: Arc<Self>,
        _item_id: ItemId,
        _action_id: ActionId,
    ) -> Result<ExecOutcome, ProviderError> {
        unreachable!("FixtureProvider::execute is never called by these tests")
    }
}

/// Builds a [`CheckedItems`] out of `items`, all agreeing with `provider_id`
/// and [`Kind::Action`] — the way a [`ResultSource`] hands a batch to the
/// daemon as checked items instead of a bare `Vec<Item>` (issue #85).
///
/// Always chunked into pieces of at most [`MAX_ITEMS_PER_PROVIDER_ANSWER`]
/// before calling [`CheckedItems::check`], rather than one `ProviderOutput`
/// for the whole of `items`: `check` truncates any *one* output over that
/// cap (issue #30/#61), so an unchunked call could not build the
/// deliberately-over-the-frame-bound batches `lifecycle.rs`'s truncation
/// tests need — see `a_list_over_the_frame_bound_is_truncated_and_terminates`
/// there. For every shorter fixture this is exactly one chunk, so it behaves
/// like an unchunked call and costs callers nothing.
///
/// Panics if `items` would not actually pass the check, since a fixture that
/// does not is a bug in the fixture, not a case a test wants to construct
/// silently.
pub fn checked_items(provider_id: &'static str, items: Vec<Item>) -> CheckedItems {
    let manifest = ProviderManifest {
        id: provider_id,
        kinds: vec![Kind::Action],
        modes: vec![Mode::All],
        min_term_len: 0,
        budget: Duration::from_millis(50),
        ids_are_safe_to_persist_in_the_clear: false,
    };
    let outputs = items
        .chunks(MAX_ITEMS_PER_PROVIDER_ANSWER)
        .map(|chunk| {
            ProviderOutput::from_provider(&FixtureProvider(manifest.clone()), chunk.to_vec())
        })
        .collect();
    let checked = CheckedItems::check(outputs);
    assert!(
        checked.rejections().is_empty(),
        "checked_items is for well-formed fixtures only — got {:?}",
        checked.rejections()
    );
    checked
}
