//! Wires the pre-built window, the IPC thread, and (for `--screenshot`) the
//! headless capture harness together into one running [`adw::Application`].
//!
//! # What "hop toggle → control message → present()" resolves to here
//!
//! §8 of the design spec describes `hop toggle` sending "a control message"
//! that a pre-built, already-running `hop-gtk` answers by presenting its
//! window. `hop toggle` itself (a `hop-cli` subcommand) is not this issue's
//! to build — `crates/hop-cli/src/lib.rs`'s own doc comment already lists it
//! among the not-yet-landed subcommands. What this issue *can* build is the
//! receiving half: the mechanism by which a second invocation of this same
//! process reaches the first one's already-built window, so that whatever
//! `hop toggle` becomes only has to run `hop-gtk` again.
//!
//! [`gio::Application`] (which [`gtk::Application`] and [`adw::Application`]
//! both build on) already *is* that mechanism, for free, once an
//! application id is registered without [`gio::ApplicationFlags::NON_UNIQUE`]:
//! the first `run()` on a machine registers as the primary instance over
//! D-Bus and runs normally; every subsequent `run()` under the same id
//! detects a primary instance is already registered, forwards an `Activate`
//! call to it over D-Bus instead of running locally, and exits. The
//! `activate` handler below fires identically either way — on this
//! process's own first run, and on every later re-invocation forwarded from
//! a second process — which is exactly the "control message → `present()`"
//! shape §8 asks for, built entirely out of GLib's own single-instance
//! machinery rather than a bespoke socket this crate would otherwise have
//! to own, secure, and keep alive itself.
//!
//! [`run_screenshot`] deliberately opts *out* of this with
//! [`gio::ApplicationFlags::NON_UNIQUE`] — seeded reasoning in that
//! function's own doc comment.
//!
//! # Activation token handoff — what is and is not wired up here
//!
//! [`ui::window::HopWindow::present_with_token`] exists and does what its
//! name says: given `Some(token)`, it sets `XDG_ACTIVATION_TOKEN` in this
//! process's environment immediately before calling `present()`, which is
//! the variable GDK's Wayland backend reads to ask the compositor for focus
//! without being treated as an unsolicited focus-steal. That covers the
//! *first* launch correctly when a compositor shortcut has already set that
//! variable in the environment `hop-gtk` was spawned with — this function
//! reads it in [`run_interactive`] below and passes it through.
//!
//! What it does **not** cover is a *second* invocation being forwarded to
//! an already-running primary instance (the common case in practice, once
//! `hop toggle` exists: the window is usually already built and hidden, not
//! freshly launched). GLib's D-Bus activation protocol has a place for this
//! — the second process's activation token travels as one of the
//! `platform_data` entries the primary instance's `GApplication` receives —
//! but reading `platform_data` means overriding `GApplication`'s
//! `activate`/`command_line` virtual methods via a `glib::subclass`, which
//! this walking skeleton does not do. This is a known, deliberate gap: the
//! structure (`present_with_token` taking an `Option<&str>`, used when one
//! is supplied) is what acceptance criterion 2 asks for, and it is real —
//! but the cross-process handoff for the re-activation case is unbuilt and
//! unverified here, not merely untested. A later issue closing it should
//! start from `GApplication::platform_data` in GIO's own documentation.

use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use gio::prelude::*;
use gtk::prelude::*;

use crate::ipc::{self, IpcEvent};
use crate::{cli, screenshot, style, ui};

/// GNOME reverse-DNS convention; unregistered (no publisher claims this
/// prefix on Flathub or similar) since v1 has not shipped anywhere that
/// would collide. Revisit alongside the release plan (design spec §12).
const APP_ID: &str = "dev.hop.Launcher";

/// How long [`run_screenshot`] waits for a driven query to finish before
/// giving up and exiting non-zero, so a `--screenshot --query ...` run
/// against an unreachable `hopd` fails promptly instead of hanging forever
/// on `ipc`'s indefinite reconnect loop (see `ipc::client::run`'s doc
/// comment for why that loop itself never gives up).
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs `hop-gtk` for `args` (`argv` with `argv[0]` already stripped) and
/// returns the process's exit code.
///
/// Resolving `hopd`'s socket path used to be this function's own
/// `socket_path()`, a private duplicate of `hop-cli`'s identical four lines
/// — that function's doc comment named exactly the condition under which
/// duplicating it a second time would stop being the right call: "were a
/// third caller to need it, the pair would be worth promoting into
/// `hop-protocol` instead of copied a second time." Issue #180 is that third
/// caller — it gives `hopd` a `--socket` override and gives one to each
/// client besides, so both existing copies had to grow the identical
/// override-resolution logic regardless — and `hop_protocol::socket::socket_path`
/// (Task 1 of that issue) is the promotion this comment predicted, called
/// here instead of `hop-gtk` growing its own second flavor of the override
/// check `hop-cli`'s `main.rs` and `hopd`'s `main.rs` both also now do.
///
/// [`cli::Args::Usage`] is handled first, before anything about `--socket`
/// is even looked at: a malformed flag (of any of the three this binary
/// now takes) is refused by [`cli::parse`] itself, and there is nothing left
/// to resolve. For [`cli::Args::Run`] and [`cli::Args::Screenshot`], the
/// `socket` field each carries is resolved right here, immediately after
/// `parse` returns (design decision D6 of issue #180's plan) — `None`
/// derives the default path exactly as before this issue, `Some` resolves
/// and constrains the override. A refusal is reported and exits through the
/// same code the `Usage` arm above already uses, per that decision: no new
/// error channel, no new exit code.
pub fn run(args: impl Iterator<Item = String>) -> ExitCode {
    let parsed = cli::parse(args);

    let socket = match &parsed {
        cli::Args::Run { socket } | cli::Args::Screenshot { socket, .. } => socket.as_deref(),
        cli::Args::Usage => {
            eprintln!("{}", cli::USAGE);
            return ExitCode::FAILURE;
        }
    };

    let socket_path = match hop_protocol::socket::socket_path(socket) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("hop-gtk: {err}");
            return ExitCode::FAILURE;
        }
    };

    // The keymap is resolved once, here, before either run mode builds a
    // window — both `run_interactive` and `run_screenshot` call
    // `ui::window::HopWindow::build`, which needs one regardless of which
    // mode is running. A [`crate::keymap::KeymapError`] refuses to start
    // `hop-gtk` at all, the same posture this function already takes one
    // check above toward a bad `--socket` override, and the same posture
    // `hopd::run` takes toward its own malformed config: see
    // `keymap`'s module doc comment, "Refusal, and what it means for
    // startup", for the full argument against starting anyway with an
    // implicit complaint logged somewhere a user is unlikely to see it.
    let keymap = match crate::keymap::Keymap::load() {
        Ok(keymap) => keymap,
        Err(err) => {
            eprintln!("hop-gtk: {err}");
            return ExitCode::FAILURE;
        }
    };

    match parsed {
        cli::Args::Run { .. } => run_interactive(socket_path, keymap),
        cli::Args::Screenshot { path, query, .. } => {
            run_screenshot(socket_path, keymap, path, query)
        }
        cli::Args::Usage => unreachable!("handled above"),
    }
}

/// `connect_startup` handler both [`run_interactive`] and [`run_screenshot`]
/// register, installing hop's own stylesheet via [`style::install`] before
/// either run's first `activate` fires (`style::install`'s own doc comment,
/// "Why `connect_startup`, not `connect_activate`", is where the deeper
/// GObject-signal-ordering argument for that hook lives; this comment is the
/// narrower one this function itself needs to make: *both* run modes call
/// it, not just one).
///
/// # Why `run_screenshot` installs it too — this was a deliberate call, not
/// an oversight
///
/// It would have been easy to skip this for `--screenshot`, on the
/// reasoning that a headless capture harness is a testing tool and testing
/// tools do not need to look pretty. That reasoning is backwards for
/// *this* harness specifically: the design spec's §11 makes `--screenshot`
/// non-optional precisely *because* it is how every future visual check of
/// this crate gets made — `tests/headless_smoke.rs`'s own module doc names
/// it as the CI-facing proof that acceptance criteria were actually met on
/// screen, and any future reviewer (human or agent) asked to eyeball a
/// capture for this issue's own visual claims would be looking at a
/// captured window. A capture of a window wearing GTK's stock Adwaita theme
/// instead of hop's own — because the one code path that loads
/// `assets/stylesheet.css` was wired into `run_interactive` only — would
/// make every one of those future checks worthless without any of them
/// failing loudly: the PNG would just quietly show the wrong thing, forever,
/// until someone thought to ask why. Installing it in both paths costs one
/// extra `connect_startup` call and keeps the harness honest about what it
/// is actually a picture of.
fn install_stylesheet(_app: &adw::Application) {
    // `connect_startup` fires only after `GtkApplication`'s own default
    // handler has already resolved a display — see `style::install`'s doc
    // comment for the `G_SIGNAL_RUN_FIRST` ordering this relies on. A
    // `None` here would mean that invariant broke, which is a programming
    // error worth failing loudly over (matching this crate's own
    // fail-loudly precedent in `tokens.rs`/`stylesheet.rs`), not an
    // ordinary, recoverable runtime condition to quietly degrade around —
    // there is no window to show, styled or not, without a display.
    let Some(display) = gtk::gdk::Display::default() else {
        panic!("hop-gtk: no gdk::Display available at GApplication startup");
    };
    style::install(&display);
}

/// The ordinary, unique-instance run: builds the window once on first
/// `activate`, presents it on every `activate` after that (this run's own
/// first activation, or a later one forwarded from a re-invocation) — see
/// this module's doc comment.
fn run_interactive(socket_path: PathBuf, keymap: crate::keymap::Keymap) -> ExitCode {
    let app = adw::Application::new(Some(APP_ID), gio::ApplicationFlags::empty());
    let activation_token = std::env::var("XDG_ACTIVATION_TOKEN").ok();

    app.connect_startup(install_stylesheet);

    app.connect_activate(move |app| {
        if let Some(existing) = app.active_window() {
            existing.present();
            return;
        }

        let (cmd_tx, evt_rx) = ipc::spawn(socket_path.clone());
        let window = ui::window::HopWindow::build(app, cmd_tx, keymap.clone());
        window.present_with_token(activation_token.as_deref());

        glib::spawn_future_local({
            let window = window.clone();
            async move {
                while let Some(event) = evt_rx.recv().await {
                    window.apply_event(event);
                }
            }
        });
    });

    // Bypasses GApplication's own argv parsing — `cli::parse` already
    // consumed this process's real arguments above, and GLib's default
    // option handling has no entries registered for `--screenshot`/`--query`
    // to recognize (this path never carries them; `run_screenshot` is a
    // separate, `NON_UNIQUE` run) but would still see `std::env::args()`
    // again here if not overridden.
    let code = app.run_with_args::<&str>(&[]);
    if code == glib::ExitCode::SUCCESS {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `hop-gtk --screenshot <path> [--query <text>]`: drives the window to the
/// state `query` describes and renders it to a PNG, then exits. Acceptance
/// criteria 7 and 8.
///
/// # Why `NON_UNIQUE`
///
/// [`run_interactive`]'s single-instance forwarding is exactly the wrong
/// behavior here: if a real `hop-gtk` were already running under [`APP_ID`],
/// a unique-instance `--screenshot` invocation would silently forward its
/// `activate` to that *other* process (which knows nothing about
/// `--screenshot` or `--query`, since GApplication's forwarding carries no
/// argv) and exit having captured nothing. [`gio::ApplicationFlags::NON_UNIQUE`]
/// makes every `--screenshot` invocation its own independent process and
/// main loop, so it is self-contained regardless of what else is running —
/// the property acceptance criterion 7 asks for ("writes a PNG ... and
/// exits", not "writes a PNG if nothing else happens to be running").
///
/// # This does not force a GDK backend — the environment's choice is honored
///
/// Acceptance criterion 8 requires this to be runnable headless in CI, with
/// no display server at all. An earlier version of this function forced
/// `GDK_BACKEND=offscreen` unconditionally, on the assumption that GTK4's
/// `offscreen` backend is always compiled in. Verifying that against this
/// issue's actual environment falsified it: Ubuntu's `libgtk-4-1` package
/// does not build the `offscreen` backend at all (only `x11`, `wayland` and
/// `broadway` — confirmed by `GDK_BACKEND=offscreen` failing with
/// `No such backend: offscreen`, and by that backend's absence from the
/// symbol table Debian's own changelog documents enabling only `broadway`
/// for), so forcing it here would have made `--screenshot` *more* fragile on
/// exactly the machine this was verified on, not less.
///
/// So this function forces nothing, and lets GDK's ordinary backend
/// selection run: whatever `GDK_BACKEND` already names, or GDK's normal
/// auto-probe (Wayland, then X11) against whatever display is actually
/// present. What was verified headless, on this machine, is:
///
/// ```sh
/// gtk4-broadwayd :0 &                       # NOT `broadwayd` — that PATH
///                                            # hit is libgtk-3-bin's server
///                                            # and speaks a different,
///                                            # incompatible protocol; GTK4
///                                            # apps need GTK4's own.
/// GDK_BACKEND=broadway BROADWAY_DISPLAY=:0 hop-gtk --screenshot out.png
/// ```
///
/// which produced a real, valid PNG of the pre-built window. A CI image that
/// happens to build (or install) a GTK4 with `offscreen` compiled in can use
/// `GDK_BACKEND=offscreen` instead, with no code change here — this function
/// does not care which headless backend is chosen, only that `GDK_BACKEND`
/// (or a real display) resolves to *something* before `app.run_with_args`
/// below opens one.
fn run_screenshot(
    socket_path: PathBuf,
    keymap: crate::keymap::Keymap,
    out_path: PathBuf,
    query: String,
) -> ExitCode {
    let app = adw::Application::new(Some(APP_ID), gio::ApplicationFlags::NON_UNIQUE);
    let exit_code = Rc::new(std::cell::Cell::new(ExitCode::FAILURE));

    app.connect_startup(install_stylesheet);

    app.connect_activate({
        let exit_code = exit_code.clone();
        move |app| {
            let (cmd_tx, evt_rx) = ipc::spawn(socket_path.clone());
            let window = ui::window::HopWindow::build(app, cmd_tx.clone(), keymap.clone());
            window.present_with_token(None);

            let done = Rc::new(std::cell::Cell::new(false));
            glib::timeout_add_local_once(SCREENSHOT_TIMEOUT, {
                let done = done.clone();
                let app = app.clone();
                move || {
                    if !done.get() {
                        eprintln!("hop-gtk: timed out waiting for a query result to screenshot");
                        app.quit();
                    }
                }
            });

            glib::spawn_future_local({
                let app = app.clone();
                let query = query.clone();
                let out_path = out_path.clone();
                let exit_code = exit_code.clone();
                async move {
                    drive_to_state(&window, &evt_rx, &query).await;
                    exit_code.set(
                        match capture_once_mapped(window.window.upcast_ref(), &out_path).await {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(err) => {
                                eprintln!("hop-gtk: screenshot failed: {err}");
                                ExitCode::FAILURE
                            }
                        },
                    );
                    done.set(true);
                    app.quit();
                }
            });
        }
    });

    app.run_with_args::<&str>(&[]);
    exit_code.get()
}

/// Retries [`screenshot::capture`] until it succeeds or [`SCREENSHOT_TIMEOUT`]
/// elapses.
///
/// A headless backend (`broadway`, `offscreen`) maps and size-allocates a
/// surface asynchronously, on the main loop's own schedule, not
/// synchronously inside `present()`. Two narrower gates were tried first and
/// both proved insufficient against a real `broadway` display (verified
/// while building this issue): `widget.width()`/`height()` report the
/// window's *requested* size immediately at construction, before anything is
/// mapped, so checking size alone returns instantly without proving anything
/// was drawn; `widget.is_mapped()` turned true well before a `GskRenderer`
/// capture actually had a node to render, still hitting
/// [`screenshot::ScreenshotError::NoRenderNode`]. Retrying the capture
/// itself, rather than trying to name the one GTK signal or property that
/// means "a `GskRenderer` snapshot will now succeed", sidesteps needing to
/// get that predicate exactly right: [`screenshot::ScreenshotError::NoRenderNode`]
/// is treated as "not ready yet" and retried on a short real delay
/// (`glib::timeout_future`, which actually yields control back to the main
/// loop for its duration — a non-blocking `MainContext::iteration(false)`
/// loop, tried before this, does nothing when nothing is already pending, so
/// spinning it a fixed number of times proves only that the loop ran, not
/// that any real time passed for the backend's frame clock to act in); every
/// other error is returned immediately, since retrying a write failure or a
/// renderer that failed to realize would not change on a second try.
async fn capture_once_mapped(
    widget: &gtk::Widget,
    out_path: &std::path::Path,
) -> Result<(), screenshot::ScreenshotError> {
    let deadline = std::time::Instant::now() + SCREENSHOT_TIMEOUT;
    loop {
        match screenshot::capture(widget, out_path) {
            Ok(()) => return Ok(()),
            Err(screenshot::ScreenshotError::NoRenderNode)
                if std::time::Instant::now() < deadline =>
            {
                glib::timeout_future(Duration::from_millis(30)).await;
            }
            Err(err) => return Err(err),
        }
    }
}

/// Drives the window to the state `query` describes, then returns:
///
/// - An empty `query` needs nothing sent — the empty-query state is
///   whatever the window already shows once presented (and however `ipc`'s
///   `Connected`/`ConnectFailed` status resolves), so this returns on the
///   very first event.
/// - A non-empty `query` is **typed into the entry** once `ipc` reports
///   `Connected`, which fires the same `connect_changed` handler a real
///   keystroke does and sends the query from there — see
///   [`HopWindow::set_query_text`] for why this drives the UI rather than
///   sending straight down the channel. This then waits for that query's
///   `QueryDone` before returning — capturing any earlier could race a
///   `Results` frame that has not arrived yet.
///
/// [`HopWindow::set_query_text`]: crate::ui::window::HopWindow::set_query_text
async fn drive_to_state(
    window: &ui::window::HopWindow,
    evt_rx: &crate::ipc::EventReceiver,
    query: &str,
) {
    let mut query_sent = query.is_empty();
    loop {
        let Some(event) = evt_rx.recv().await else {
            return;
        };
        let is_connected = matches!(event, IpcEvent::Connected);
        let is_query_done = matches!(event, IpcEvent::QueryDone);
        window.apply_event(event);

        if is_connected && !query_sent {
            window.set_query_text(query);
            query_sent = true;
            continue;
        }
        if query.is_empty() || (query_sent && is_query_done) {
            return;
        }
    }
}
