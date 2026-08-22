//! The grab loop itself: one X connection, one signal descriptor, one
//! blocking `poll` — the sync, no-runtime shape §3's salvage manifest calls
//! for ("X11 grab loop … backoff logic"), and the same reasoning
//! `hop-cli`'s "Why no tokio" doc carries: this process has exactly two
//! things to wait on and nothing to schedule concurrently, so an async
//! runtime would be a scheduler with nothing to schedule.
//!
//! # The loop's shape
//!
//! ```text
//! block SIGINT/SIGTERM → signalfd          (install_signal_fd)
//! loop {
//!     connect to $DISPLAY                  (retrying with backoff)
//!     resolve keysyms → keycodes           (GetKeyboardMapping)
//!     XGrabKey each configured binding     (BadAccess ⇒ single-instance exit)
//!     poll([X socket, signalfd])           (1 s tick)
//!       ├─ signalfd readable ⇒ exit 0      (AC 1)
//!       ├─ KeyPress matching a binding ⇒ spawn `hop toggle` detached
//!       └─ X socket died ⇒ log, back off, reconnect, re-grab
//! }
//! ```
//!
//! # Why a signalfd rather than a signal handler
//!
//! A handler that sets a flag leaves the race where the flag is set between
//! the flag-check and the `poll` — the classic lost-wakeup that makes a
//! daemon ignore its own shutdown signal for as long as the X server stays
//! quiet. Blocking the signals (`sigprocmask`) and reading them back
//! through a descriptor closes it structurally: the signal becomes *data*
//! the same `poll` that watches the X socket wakes on, so there is no
//! window in which a SIGTERM can be delivered but not observed, and no
//! async-signal handler anywhere in the process. The cost is four narrow
//! `unsafe` touchpoints around `libc` calls this workspace's
//! `unsafe_code = deny` lint requires declaring — the signalfd setup, the
//! `poll` wrapper, the child-side mask reset, and the `pre_exec`
//! registration that calls it — each carrying its own SAFETY comment or
//! `reason` below.
//!
//! # Single-instance by X-level evidence, not lockfiles (AC 5)
//!
//! Two `hop-hotkeyd`s racing for the same binding both call `XGrabKey`; the
//! protocol answers the loser with a `BadAccess` error — the server itself
//! arbitrates, so there is no lockfile to go stale when a session dies
//! mid-flight, no PID file to misread after a PID reuse, and no window
//! between "checked" and "grabbed" in which a third instance can sneak in.
//! The loser prints which binding was already held and exits non-zero.
//!
//! # Backoff
//!
//! The salvaged branch's backoff logic, kept deliberately boring: every
//! failed or broken connection waits twice as long as the previous one,
//! from 500 ms out to a 30 s ceiling, before trying again. A resident agent
//! that dies because the X server restarted (logging out and back in) would
//! be exactly the "optional enhancement" silently vanishing; one that spins
//! on a busy retry loop against a dead socket would be burning a core to do
//! it. Exponential-with-ceiling is the smallest thing that avoids both.

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

// The `Connection` trait supplies `setup`/`poll_for_event`/`flush`;
// `ConnectionExt` carries the core-protocol requests (`grab_key`,
// `get_keyboard_mapping`). The concrete connection `x11rb::connect` hands
// back is a `RustConnection`, named because `resolve_keycodes` takes it by
// reference.
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::rust_connection::RustConnection;

use crate::config::ToggleEntry;

/// Everything [`run`] can conclude, mapped to exit codes by its caller.
enum Outcome {
    /// A blocked signal arrived on the signalfd: shut down cleanly.
    Signalled,
    /// Another client already holds one of our grabs — the single-instance
    /// evidence. Carries the spelling for the message.
    AlreadyHeld(String),
    /// Something no retry can fix (a configured keysym this keyboard does
    /// not have). Carries the reason.
    Fatal(String),
    /// The X connection went away mid-loop (or was never there); the caller
    /// backs off and reconnects.
    ConnectionLost(String),
}

/// Blocks SIGINT/SIGTERM process-wide and returns a descriptor that becomes
/// readable when either arrives. Returned as a [`File`] — the `OwnedFd` the
/// raw call produces, wrapped so the loop can both `poll` its raw
/// descriptor and `read` the payload with safe code.
///
/// Blocking must happen *before* the descriptor is created and before the
/// grab loop starts: a signal delivered in between would take the default
/// disposition (kill the process — acceptable but ungraceful) or, worse,
/// arrive after the mask is set but be queued invisibly. With the mask set
/// first, delivery is deferred until the `read` below asks for it.
#[expect(
    unsafe_code,
    reason = "signalfd(2) is the only way to read blocked signals as data; \
              sigprocmask and signalfd report failure by return value, and \
              the produced fd is transferred whole into an OwnedFd/File"
)]
fn install_signal_fd() -> io::Result<File> {
    // SAFETY: `sigset_t` is a plain C value that `sigemptyset`/`sigaddset`
    // initialize; `sigprocmask` and `signalfd` report failure by return
    // value, checked immediately. Blocking SIGINT/SIGTERM changes this
    // process's own signal disposition only — the signals are still
    // deliverable through the returned descriptor, which is the point.
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        if libc::sigemptyset(&mut mask) != 0
            || libc::sigaddset(&mut mask, libc::SIGINT) != 0
            || libc::sigaddset(&mut mask, libc::SIGTERM) != 0
        {
            return Err(io::Error::last_os_error());
        }
        if libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = libc::signalfd(-1, &mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor owned by this call — created
        // by the `signalfd` above, never duplicated or stored elsewhere —
        // so transferring it into an `OwnedFd` cannot double-close
        // anything, and the `File` wrap takes that ownership over whole.
        Ok(File::from(OwnedFd::from_raw_fd(fd)))
    }
}

/// Waits until one of `fds` is readable (or `timeout` elapses), returning
/// whether anything woke early. `POLLERR`/`POLLHUP` count as wakeups: a
/// hung-up X socket must end the wait so the loop can reconnect, not sit
/// in another full timeout.
#[expect(
    unsafe_code,
    reason = "poll(2) has no safe wrapper in this workspace's dependency \
              set, and it is what lets one blocking call watch both the X \
              socket and the signalfd; failure is reported by return value"
)]
fn wait_readable(fds: &mut [libc::pollfd], timeout: Duration) -> io::Result<bool> {
    // SAFETY: `fds` is a valid slice of initialized `pollfd` values for the
    // duration of the call; `poll` reports failure by return value.
    let woke = unsafe {
        libc::poll(
            fds.as_mut_ptr(),
            fds.len() as libc::nfds_t,
            timeout.as_millis() as libc::c_int,
        )
    };
    if woke < 0 {
        // A blocked-and-deferred signal cannot interrupt this call (they
        // are masked), so EINTR is not expected; treat it as a spurious
        // wakeup anyway rather than panicking on a benign errno.
        if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            return Ok(true);
        }
        return Err(io::Error::last_os_error());
    }
    Ok(woke > 0)
}

/// How long to wait before connection attempt `attempt` (1-based): doubling
/// from 500 ms, capped at 30 s — see this module's "Backoff".
fn backoff(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(6);
    Duration::from_millis(500)
        .saturating_mul(1 << shift)
        .min(Duration::from_secs(30))
}

/// Runs the resident hotkey agent over `bindings` until a signal ends it.
///
/// Backend selection comes first, in issue #235's documented order —
/// **GlobalShortcuts portal → X11 grab → DE-shortcut guidance** (the spec
/// assigns these per-platform at §3; the *order* is the issue's own,
/// grounded in §2's graceful-degradation rule) — and the chosen backend
/// plus the reason is logged before anything grabs anything. Every probe
/// failure degrades to the next backend with the reason logged; the
/// guidance arm prints the per-desktop one-liners and exits 0, the same
/// logged-no-op posture missing or malformed config gets.
///
/// Returns the process's exit code rather than a `Result`: every terminal
/// outcome here is either "clean shutdown" (exit 0) or "refused with a
/// printed reason" (exit 1), and nothing upstream can act on finer detail.
pub fn run(bindings: &[ToggleEntry]) -> ExitCode {
    let mut signal_fd = match install_signal_fd() {
        Ok(fd) => fd,
        Err(err) => {
            eprintln!("hop-hotkeyd: cannot set up signal handling: {err}");
            return ExitCode::FAILURE;
        }
    };

    match select_backend(bindings) {
        Backend::Portal(session) => portal_arm(signal_fd, session),
        Backend::X11Grab => {
            let mut attempt = 0u32;
            loop {
                match session(&mut signal_fd, bindings) {
                    Outcome::Signalled => {
                        eprintln!("hop-hotkeyd: exiting on signal");
                        return ExitCode::SUCCESS;
                    }
                    Outcome::AlreadyHeld(spelling) => {
                        eprintln!(
                            "hop-hotkeyd: `{spelling}` is already grabbed by another client \
                             — is another hop-hotkeyd running?"
                        );
                        return ExitCode::FAILURE;
                    }
                    Outcome::Fatal(reason) => {
                        eprintln!("hop-hotkeyd: {reason}");
                        return ExitCode::FAILURE;
                    }
                    Outcome::ConnectionLost(reason) => {
                        attempt += 1;
                        let delay = backoff(attempt);
                        eprintln!(
                            "hop-hotkeyd: {reason}; retrying in {:.1}s",
                            delay.as_secs_f64()
                        );
                        std::thread::sleep(delay);
                    }
                }
            }
        }
        Backend::Guidance => {
            print_guidance();
            ExitCode::SUCCESS
        }
    }
}
/// The portal arm once selected: block on the session's `Activated`
/// signal forever, running the universal toggle per activation.
///
/// Shutdown rides the signalfd on a watcher thread rather than the main
/// thread's `poll`: the main thread is blocked inside zbus's message
/// iterator, which offers no descriptor to multiplex, and a second
/// runtime to poll both is precisely the dependency this crate refuses.
/// With SIGINT/SIGTERM blocked process-wide ([`install_signal_fd`]), the
/// signal sits pending until the watcher reads it — no lost-wakeup window —
/// and `process::exit` ends the daemon cleanly; the spawned toggles are
/// in their own process groups and outlive us by design.
fn portal_arm(signal_fd: File, session: crate::portal::PortalSession) -> ExitCode {
    std::thread::spawn(move || {
        let mut signal_fd = signal_fd;
        // The descriptor [`install_signal_fd`] hands out is non-blocking
        // (the X11 arm multiplexes it through `poll`), so this arm polls
        // too: a bare `read` would return `EAGAIN` when *no* signal is
        // pending and be indistinguishable from one. A 1 s tick costs
        // nothing and keeps the exit path identical to the X11 arm's.
        let mut fds = [libc::pollfd {
            fd: signal_fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        loop {
            fds[0].revents = 0;
            match wait_readable(&mut fds, Duration::from_secs(1)) {
                Ok(true) => {
                    let mut info = [0u8; 128];
                    match signal_fd.read(&mut info) {
                        Ok(n) if n > 0 => {
                            eprintln!("hop-hotkeyd: exiting on signal");
                            std::process::exit(0);
                        }
                        _ => continue, // spurious wakeup; keep waiting
                    }
                }
                _ => continue,
            }
        }
    });
    if let Err(reason) = crate::portal::serve(session, spawn_toggle) {
        // The portal or the bus went away mid-session. Exiting non-zero
        // hands the decision to the supervisor: systemd restarts us and
        // startup selection runs again against whatever is actually there,
        // which is the same degradation path a fresh login takes.
        eprintln!("hop-hotkeyd: {reason}; exiting for the supervisor to re-select a backend");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// The per-session DE-shortcut guidance (issue #235's criterion 4): the
/// one-liners that make a desktop's own custom shortcut run the universal
/// toggle, for the sessions where neither automatic backend applies.
fn print_guidance() {
    eprintln!(
        "hop-hotkeyd: no automatic backend applies — configure your desktop's \
         own shortcut to run `hop toggle`:"
    );
    eprintln!(
        "hop-hotkeyd:   GNOME: Settings → Keyboard → View and Customize Shortcuts \
         → Custom Shortcuts → add one with the command `hop toggle`"
    );
    eprintln!(
        "hop-hotkeyd:   KDE Plasma: System Settings → Shortcuts → Add New → \
         Command or Script, `hop toggle`"
    );
    eprintln!(
        "hop-hotkeyd:   sway/wlroots: add `bind = SUPER, Space, exec, hop toggle` \
         to your sway config"
    );
}
/// Which backend won selection — the session-less shape the pure decision
/// core ([`decide`]) returns. [`select_backend`] pairs [`Choice::Portal`]
/// with the live session its probe produced.
enum Choice {
    Portal,
    X11Grab,
    Guidance,
}

/// The backends issue #235's order walks, in the state the selected arm
/// needs them: the portal arm carries its live session, the X11 arm
/// re-enters the existing grab loop, and the guidance arm only prints.
enum Backend {
    Portal(crate::portal::PortalSession),
    X11Grab,
    Guidance,
}

/// How the portal probe went. Kept separate from the live session object
/// so the ordering decision itself is unit-testable with neither a bus nor
/// an X server ([`decide`]); `select_backend` keeps the session alongside.
enum PortalVerdict {
    Bound,
    /// Nothing to talk to: no session bus, or no service owning the
    /// portal's well-known name.
    Unavailable(String),
    /// The portal answered but said no — a refused bind, a malformed
    /// reply, a timeout waiting for its verdict.
    Refused(String),
}

/// Walks issue #235's documented order — **portal → X11 grab → guidance** —
/// over probe outcomes and produces both the choice and the exact log lines
/// that explain it (the caller prints each with the crate's `hop-hotkeyd:`
/// prefix; the stable phrasing is what `hop doctor`'s M6 report will grep).
///
/// Pure and unit-tested below. `x11` is a closure rather than a result so
/// the X server is probed only after the portal has fallen through — on a
/// working portal no X connection is ever attempted (spec §2's rule that
/// every capability probe has a defined fallback is *why* each arm names
/// its reason rather than failing quietly).
fn decide(
    portal: PortalVerdict,
    x11: impl FnOnce() -> Result<(), String>,
) -> (Choice, Vec<String>) {
    let mut lines = Vec::new();
    match portal {
        PortalVerdict::Bound => {
            lines.push(
                "backend portal chosen: org.freedesktop.portal.Desktop accepted \
                 CreateSession/BindShortcuts"
                    .to_string(),
            );
            return (Choice::Portal, lines);
        }
        PortalVerdict::Unavailable(reason) => {
            lines.push(format!(
                "backend portal unavailable: {reason}; falling back to the X11 grab"
            ));
        }
        PortalVerdict::Refused(reason) => {
            lines.push(format!(
                "backend portal bind refused: {reason}; falling back to the X11 grab"
            ));
        }
    }
    match x11() {
        Ok(()) => {
            lines.push("backend X11 grab chosen: an X display is reachable".to_string());
            (Choice::X11Grab, lines)
        }
        Err(reason) => {
            lines.push(format!("backend X11 grab unavailable: {reason}"));
            lines.push(
                "no automatic backend applies; printing per-desktop shortcut \
                 guidance instead"
                    .to_string(),
            );
            (Choice::Guidance, lines)
        }
    }
}

/// Runs the real probes in the documented order and returns the selected
/// backend. The portal verdict and its session travel together: `session`
/// is `Some` exactly when the verdict is [`PortalVerdict::Bound`].
fn select_backend(bindings: &[ToggleEntry]) -> Backend {
    let (verdict, session) = match crate::portal::probe() {
        Err(reason) => (PortalVerdict::Unavailable(reason), None),
        Ok(conn) => match crate::portal::bind(&conn, bindings) {
            Ok(session) => (PortalVerdict::Bound, Some(session)),
            Err(reason) => (PortalVerdict::Refused(reason), None),
        },
    };
    let (choice, lines) = decide(verdict, probe_x11);
    for line in &lines {
        eprintln!("hop-hotkeyd: {line}");
    }
    match choice {
        Choice::Portal => Backend::Portal(
            // By construction: `Bound` is only ever produced with a live
            // session riding next to it in `select_backend`.
            session.expect("a Bound portal verdict always carries its session"),
        ),
        Choice::X11Grab => Backend::X11Grab,
        Choice::Guidance => Backend::Guidance,
    }
}

/// The X11 reachability probe: one throwaway connection attempt. The
/// winning X11 arm re-connects inside [`session`], which keeps that
/// function's reconnect-and-re-grab loop untouched by selection.
fn probe_x11() -> Result<(), String> {
    x11rb::connect(None)
        .map(|_| ())
        .map_err(|err| format!("cannot connect to the X server ({err}); is DISPLAY set?"))
}

/// One connect → grab → serve cycle. Ends when a signal arrives, a grab
/// conflicts, something fatal is diagnosed, or the connection drops (the
/// caller reconnects).
fn session(signal_fd: &mut File, bindings: &[ToggleEntry]) -> Outcome {
    let (conn, screen_num) = match x11rb::connect(None) {
        Ok(conn) => conn,
        Err(err) => {
            return Outcome::ConnectionLost(format!(
                "cannot connect to the X server ({err}); is DISPLAY set?"
            ));
        }
    };
    let root = conn.setup().roots[screen_num].root;

    // Keysym → keycode is a property of *this server's* keyboard mapping,
    // resolved once per connection (a reconnect may land on a rebuilt
    // keymap and must re-resolve, which falling through the outer loop
    // gives for free).
    let resolved = match resolve_keycodes(&conn, bindings) {
        Ok(resolved) => resolved,
        Err(reason) => return Outcome::Fatal(reason),
    };

    for entry in &resolved {
        // The reply is what carries BadAccess — the server's verdict on
        // whether anyone else already holds this combination (AC 5) — so
        // the request is not fire-and-forget: `.reply()` both forces the
        // round trip and delivers the verdict.
        let cookie = match conn.grab_key(
            false,
            root,
            entry.entry.binding.modifiers.into(),
            entry.keycode,
            x11rb::protocol::xproto::GrabMode::ASYNC,
            x11rb::protocol::xproto::GrabMode::ASYNC,
        ) {
            Ok(cookie) => cookie,
            Err(err) => return Outcome::ConnectionLost(format!("XGrabKey failed: {err}")),
        };
        if let Err(err) = cookie.check() {
            match err {
                x11rb::errors::ReplyError::X11Error(error)
                    if error.error_kind == x11rb::protocol::ErrorKind::Access =>
                {
                    return Outcome::AlreadyHeld(entry.entry.spelling.clone());
                }
                err => return Outcome::ConnectionLost(format!("XGrabKey failed: {err}")),
            }
        }
        eprintln!("hop-hotkeyd: grabbed {}", entry.entry.spelling);
    }

    // The event loop proper: one poll over both descriptors, forever.
    let mut fds = [
        libc::pollfd {
            fd: conn.stream().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: signal_fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        fds.iter_mut().for_each(|entry| entry.revents = 0);
        if let Err(err) = wait_readable(&mut fds, Duration::from_secs(1)) {
            return Outcome::ConnectionLost(format!("waiting for events failed: {err}"));
        }

        if fds[1].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
            // Whatever the signal was, it is one of the two we asked for;
            // read the payload to consume it and end the session cleanly.
            let mut info = [0u8; 128];
            let _ = signal_fd.read(&mut info);
            return Outcome::Signalled;
        }

        if fds[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
            loop {
                match conn.poll_for_event() {
                    Ok(Some(event)) => handle_event(&event, &resolved),
                    Ok(None) => break,
                    Err(err) => {
                        return Outcome::ConnectionLost(format!("lost the X connection: {err}"));
                    }
                }
            }
            if let Err(err) = conn.flush() {
                return Outcome::ConnectionLost(format!("lost the X connection: {err}"));
            }
        }
    }
}

/// One binding resolved against the live server's keyboard mapping.
struct ResolvedBinding<'a> {
    /// The config entry, borrowed for its spelling and modifier mask.
    entry: &'a ToggleEntry,
    /// The keycode carrying [`Self::entry`]'s keysym on this server.
    keycode: u8,
}

/// Maps every binding's keysym to a keycode via `GetKeyboardMapping`.
///
/// The mapping is scanned linearly per keysym and the *first* keycode
/// carrying it wins — on a default keymap each Latin-1 keysym sits on
/// exactly one keycode, and where a key genuinely appears twice (some
/// layouts duplicate modifiers) either choice grabs the same physical
/// press. A keysym the server does not carry at all is fatal with a named
/// reason: grabbing everything *except* the user's configured key and
/// running anyway would look healthy while doing nothing.
fn resolve_keycodes<'a>(
    conn: &RustConnection,
    bindings: &'a [ToggleEntry],
) -> Result<Vec<ResolvedBinding<'a>>, String> {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let reply = conn
        .get_keyboard_mapping(min, max - min + 1)
        .map_err(|err| format!("GetKeyboardMapping failed: {err}"))?
        .reply()
        .map_err(|err| format!("GetKeyboardMapping failed: {err}"))?;
    let per_keycode = usize::from(reply.keysyms_per_keycode).max(1);

    let mut resolved = Vec::with_capacity(bindings.len());
    for entry in bindings {
        let keycode = reply
            .keysyms
            .chunks(per_keycode)
            .enumerate()
            .find(|(_, group)| group.contains(&entry.binding.keysym))
            .map(|(index, _)| min + index as u8);
        match keycode {
            Some(keycode) => resolved.push(ResolvedBinding { entry, keycode }),
            None => {
                return Err(format!(
                    "`{}` resolves to keysym {:#x}, which this keyboard's mapping does not offer",
                    entry.spelling, entry.binding.keysym
                ));
            }
        }
    }
    Ok(resolved)
}

/// Acts on one X event: a `KeyPress` whose keycode and (forgivingly
/// compared) modifier state match a resolved binding spawns the universal
/// toggle. Every other event — releases, `MappingNotify`, whatever else a
/// server cares to send — is ignored.
fn handle_event(event: &x11rb::protocol::Event, resolved: &[ResolvedBinding<'_>]) {
    let x11rb::protocol::Event::KeyPress(press) = event else {
        return;
    };
    for entry in resolved {
        if press.detail == entry.keycode
            && u16::from(press.state) & !FORGIVEN_STATE == entry.entry.binding.modifiers
        {
            spawn_toggle();
            return;
        }
    }
}

/// Modifier bits the matcher forgives when comparing a `KeyPress`'s state
/// against a binding's required modifiers: `LockMask` (Caps Lock's latched
/// state, which X ORs into every key event while held — the same hazard
/// `hop-gtk::keymap`'s `relevant_modifiers` documents for the same reason)
/// and `Mod2Mask` (Num Lock, same story one row over). Without this, a
/// user with Num Lock on would find their hotkey dead for no reason either
/// they or the config can see.
///
/// Also the anchor for the child-side mask reset below: everything between
/// this constant and [`spawn_toggle`] is the signal story of this module.
const FORGIVEN_STATE: u16 = 0x0002 | 0x0010;

/// Resets this process's signal mask to empty.
///
/// Never called by hop-hotkeyd itself — this process *wants* its signals
/// blocked. It is installed into the spawned toggle's `pre_exec`, because a
/// fork inherits the parent's signal mask: without this, `hop toggle` (and,
/// through it, every hop-gtk it launches) would inherit the blocked
/// SIGINT/SIGTERM and ignore Ctrl+C and SIGTERM for as long as it ran.
#[expect(
    unsafe_code,
    reason = "sigset_t is a plain C value only sigemptyset can portably \
              initialize, and sigprocmask is the only way to change the \
              mask; both calls report failure by return value"
)]
fn reset_signal_mask() -> io::Result<()> {
    // SAFETY: both calls are async-signal-safe (POSIX.1-2008's required
    // list), which is the contract `pre_exec` imposes on everything the
    // closure touches between fork and exec; neither has preconditions on
    // process state beyond the initialized `sigset_t`.
    unsafe {
        let mut empty: libc::sigset_t = std::mem::zeroed();
        if libc::sigemptyset(&mut empty) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Spawns `hop toggle` — the universal toggle, design spec §3 — detached
/// from this process so hop-gtk's re-invocation (which forwards to the
/// resident instance and exits) is never reaped by us mid-handshake.
///
/// `process_group(0)` puts the child in its own process group: a Ctrl+C
/// aimed at a foreground group containing hop-hotkeyd must not also land
/// in the freshly-spawned toggle. Failure to spawn is logged, never
/// fatal — the grab loop's job is to keep holding the key even when the
/// toggle side is briefly broken.
fn spawn_toggle() {
    let mut command = Command::new("hop");
    command.arg("toggle");

    #[expect(
        unsafe_code,
        reason = "CommandExt::pre_exec is the only hook that runs between \
                  fork and exec; registering the mask reset there is the \
                  only way to keep the blocked SIGINT/SIGTERM out of the \
                  child"
    )]
    // SAFETY: the closure runs once per spawn, between fork and exec, and
    // calls only `reset_signal_mask` — async-signal-safe libc calls, which
    // is exactly what `pre_exec`'s own documentation requires of the
    // closure. Registration itself cannot fail; a failing closure surfaces
    // through `spawn` below.
    let spawned = unsafe { command.pre_exec(reset_signal_mask) }
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    if let Err(err) = spawned {
        eprintln!("hop-hotkeyd: could not run `hop toggle`: {err}");
    }
}
/// The selection-order units: [`decide`] is pure, so the documented
/// fallback order and its log phrasing are pinned here without a bus, a
/// portal, or an X server. The round trips themselves are `tests/portal.rs`'s
/// business.
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// The X11 probe closure tests use when X *is* reachable — records that
    /// it ran at all, since on a working portal it must never be called.
    fn x11_ok(flag: &mut bool) -> impl FnOnce() -> Result<(), String> + '_ {
        move || {
            *flag = true;
            Ok(())
        }
    }

    #[test]
    fn a_working_portal_wins_and_never_probes_x11() {
        let mut x11_probed = false;
        let (choice, lines) = decide(PortalVerdict::Bound, x11_ok(&mut x11_probed));
        assert!(matches!(choice, Choice::Portal));
        assert!(!x11_probed, "a working portal must short-circuit selection");
        let joined = lines.join("\n");
        assert!(
            joined.contains("backend portal chosen"),
            "the chosen-backend line is what startup logs: {joined}"
        );
    }

    #[test]
    fn no_portal_falls_through_to_x11_with_the_reason_logged() {
        let (choice, lines) = decide(
            PortalVerdict::Unavailable("no session bus (test)".to_string()),
            || Ok(()),
        );
        assert!(matches!(choice, Choice::X11Grab));
        let joined = lines.join("\n");
        for expected in [
            "backend portal unavailable",
            "no session bus (test)",
            "falling back to the X11 grab",
            "backend X11 grab chosen",
        ] {
            assert!(
                joined.contains(expected),
                "missing `{expected}` in: {joined}"
            );
        }
    }

    #[test]
    fn a_bind_refusal_degrades_exactly_like_an_absent_portal() {
        let (choice, lines) = decide(
            PortalVerdict::Refused("BindShortcuts refused (response code 1)".to_string()),
            || Err("cannot connect to the X server (test); is DISPLAY set?".to_string()),
        );
        assert!(matches!(choice, Choice::Guidance));
        let joined = lines.join("\n");
        // Criterion 3's wording split: a refusal is reported as a refused
        // bind, not as an unavailable portal — the reasons are different
        // even though both degrade.
        for expected in [
            "backend portal bind refused",
            "BindShortcuts refused (response code 1)",
            "falling back to the X11 grab",
            "backend X11 grab unavailable",
            "cannot connect to the X server (test)",
            "no automatic backend applies",
        ] {
            assert!(
                joined.contains(expected),
                "missing `{expected}` in: {joined}"
            );
        }
    }

    #[test]
    fn guidance_is_reached_only_after_both_backends_decline() {
        let x11_probed = std::cell::Cell::new(false);
        let (choice, lines) = decide(
            PortalVerdict::Unavailable("test".to_string()),
            || -> Result<(), String> {
                x11_probed.set(true);
                Err("no X".to_string())
            },
        );
        assert!(matches!(choice, Choice::Guidance));
        assert!(x11_probed.get());
        assert_eq!(lines.len(), 3, "one reason per probe plus the outcome");
    }
}
