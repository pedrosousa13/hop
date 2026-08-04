//! The apps provider through the daemon, over a real socket: acceptance
//! criterion 7 on issue #57. `apps.rs`'s own unit and `watcher_tests`
//! modules cover the provider's units and the watcher directly; this file
//! covers what a client actually receives.
//!
//! Plain `#[test]` functions driving a blocking
//! `std::os::unix::net::UnixStream` client, matching `lifecycle.rs`'s and
//! `host.rs`'s shape — no `#[tokio::test]` client in this crate's suites,
//! and inventing one here would be a second harness where `tests/common`
//! exists to prevent exactly that.

#![allow(clippy::unwrap_used)]

mod common;

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use common::{hello, recv, send, start_daemon};
use hop_core::host::{HostPolicy, ProviderHost};
use hop_protocol::{ClientMsg, DaemonMsg, QueryText};
use hopd::apps::{AppIndex, AppsProvider, EmptyWindowSource, SystemLauncher, scan_apps};
use hopd::source::HostSource;

/// Writes one `.desktop` file into `dir`.
fn write_entry(dir: &std::path::Path, file_name: &str, name: &str) {
    std::fs::write(
        dir.join(file_name),
        format!("[Desktop Entry]\nName={name}\nExec={name}\n"),
    )
    .unwrap();
}

/// A daemon serving a `ProviderHost` with one `AppsProvider` registered over
/// `roots`, built the same way `hopd::apps::build_apps_provider` builds the
/// real one — minus the environment read, since the roots are the test's
/// own tempdir rather than the process's real XDG state.
fn daemon_over(roots: Vec<std::path::PathBuf>) -> common::TestDaemon {
    let index = Arc::new(AppIndex::new(scan_apps(&roots)));
    hopd::apps::spawn_index_watcher(index.clone(), roots);
    let provider = AppsProvider::new(index, Arc::new(EmptyWindowSource), Arc::new(SystemLauncher));

    let mut host = ProviderHost::new(HostPolicy::default(), Arc::new(hop_core::host::NoopLog));
    host.register(provider).unwrap();
    start_daemon(HostSource::new(Arc::new(host)))
}

fn connect(daemon: &common::TestDaemon) -> UnixStream {
    let mut stream = UnixStream::connect(&daemon.socket_path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    hello(&mut stream);
    stream
}

#[test]
fn a_query_over_the_socket_returns_a_real_installed_application() {
    let dir = tempfile::tempdir().unwrap();
    write_entry(dir.path(), "firefox.desktop", "hop-e2e-canary-27b4d0");
    let daemon = daemon_over(vec![dir.path().to_path_buf()]);
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 1,
            text: QueryText::new("hop-e2e-canary-27b4d0").unwrap(),
        },
    );

    let mut items = Vec::new();
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results {
                query_id: 1,
                items: batch,
                ..
            } => items.extend(batch),
            DaemonMsg::QueryDone { query_id: 1 } => break,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "hop-e2e-canary-27b4d0");
    assert_eq!(items[0].provider, hop_core::provider::APPS_PROVIDER_ID);
}

#[test]
fn the_a_prefix_reaches_the_apps_provider_exclusively() {
    let dir = tempfile::tempdir().unwrap();
    write_entry(dir.path(), "firefox.desktop", "hop-e2e-canary-27b4d0");
    let daemon = daemon_over(vec![dir.path().to_path_buf()]);
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 2,
            text: QueryText::new("a hop-e2e-canary-27b4d0").unwrap(),
        },
    );

    let mut items = Vec::new();
    loop {
        match recv(&mut stream) {
            DaemonMsg::Results {
                query_id: 2,
                items: batch,
                ..
            } => items.extend(batch),
            DaemonMsg::QueryDone { query_id: 2 } => break,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(
        items.len(),
        1,
        "the `a ` prefix must still reach the apps provider"
    );
    assert_eq!(items[0].title, "hop-e2e-canary-27b4d0");
}

#[test]
fn a_query_that_matches_nothing_still_reaches_a_clean_query_done() {
    let dir = tempfile::tempdir().unwrap();
    write_entry(dir.path(), "firefox.desktop", "hop-e2e-canary-27b4d0");
    let daemon = daemon_over(vec![dir.path().to_path_buf()]);
    let mut stream = connect(&daemon);

    send(
        &mut stream,
        &ClientMsg::Query {
            id: 3,
            text: QueryText::new("hop-e2e-canary-nonexistent-9f1a").unwrap(),
        },
    );

    let frame = recv(&mut stream);
    assert_eq!(frame, DaemonMsg::QueryDone { query_id: 3 });
}

#[test]
fn installing_an_app_while_the_daemon_is_running_is_reflected_in_the_next_query() {
    // The strongest available proof, combining acceptance criteria 1, 2 and
    // 7 in one test: a filesystem change, observed through a live daemon,
    // over the real socket, with no restart anywhere in the test.
    let dir = tempfile::tempdir().unwrap();
    let daemon = daemon_over(vec![dir.path().to_path_buf()]);

    write_entry(dir.path(), "newapp.desktop", "hop-e2e-canary-newapp-71cd");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut found = false;
    let mut next_id = 10u64;
    while std::time::Instant::now() < deadline && !found {
        let mut stream = connect(&daemon);
        send(
            &mut stream,
            &ClientMsg::Query {
                id: next_id,
                text: QueryText::new("hop-e2e-canary-newapp-71cd").unwrap(),
            },
        );
        loop {
            match recv(&mut stream) {
                DaemonMsg::Results { items, .. } => {
                    found = items
                        .iter()
                        .any(|i| i.title == "hop-e2e-canary-newapp-71cd");
                }
                DaemonMsg::QueryDone { .. } => break,
                other => panic!("unexpected frame: {other:?}"),
            }
        }
        next_id += 1;
        if !found {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    assert!(
        found,
        "an app installed after the daemon started must be found without a restart"
    );
}
