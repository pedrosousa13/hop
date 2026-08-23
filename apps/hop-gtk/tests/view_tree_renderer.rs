//! Unit-level proof of issue #181's view-tree dispatch seam: `ui::view`'s
//! `Node` enum, the `gtk::Stack`-based dispatch container `setup` builds,
//! and the `bind`/`unbind` functions that select and populate its pages.
//!
//! Issue #190 extended this file's single `#[test]` with the icon coverage
//! its own brief asks for: both `IconSpec` arms, each arm's failure path,
//! `icon: None`'s blank slot, and the row's layout holding fixed across
//! every one of those — see the "--- issue #190" section inside
//! [`run_assertions`] below for all of it, structured as one long sequence
//! of binds against the *same* slot so every assertion in that section also
//! keeps re-proving recycling (acceptance criterion 5): no bind in the
//! whole sequence ever gets a different widget instance than the one
//! before it.
//!
//! Issue #93 further extended the "--- issue #190" section's `Path`-arm
//! coverage with the icon allow-list `ui::row::load_path_texture` now
//! enforces (`icon_roots::ALLOWED_ICON_ROOTS`, in the crate's own
//! `src/icon_roots.rs`): a path resolving outside every allowed icon root,
//! a symlink under an allowed root that leads outside one, an ordinary
//! themed icon reached through a symlink (still opens — the case that
//! makes `O_NOFOLLOW` the wrong tool), and `/proc/self/mem` named
//! literally. Doing that honestly, rather than by weakening the check for
//! the test, required changing where this file's own fixture icon files
//! live: they used to sit in a bare `tempfile::tempdir()`, which the
//! allow-list now refuses on principle since nothing designates an
//! arbitrary tempdir an icon root. This file's `#[test]` function now
//! creates a *second* tempdir before re-exec'ing its child and hands it to
//! the child as `XDG_DATA_HOME` (`Command::env`, which sets a child's
//! environment and needs no `unsafe`, unlike mutating this process's own —
//! see the section below), so the child's own `icon_roots::from_env`-driven
//! computation genuinely includes it as an allowed root, rather than a
//! test-only bypass of the real check. [`run_assertions`] then creates
//! `$XDG_DATA_HOME/icons` inside it — the exact subdirectory
//! `icon_roots::icon_roots` derives from that variable — and every fixture
//! file the `Path`-arm coverage needs now lives there, alongside a handful
//! deliberately placed outside it for the negative cases. The outer
//! `#[test]` function holds the `TempDir` alive for the whole of the
//! child's lifetime, exactly as [`BroadwayServer`] is held alive across the
//! same `Command::output()` call.
//!
//! This is a different proof than `tests/headless_smoke.rs`'s: that test
//! captures a rendered PNG and diffs two frames, which can show a title
//! painted on screen but cannot show *which widget instance* is on screen,
//! or *which named page* of a `gtk::Stack` is currently selected. Criterion
//! 5 (recycling) and D4's "same widget instance across two binds" claim are
//! about identity and structure, not pixels, so this file drives the actual
//! `gtk::Stack`/`gtk::ListItem` objects `ui::view` builds and inspects them
//! directly.
//!
//! # Why this file re-execs itself as a subprocess rather than calling
//! `gtk::init()` straight from `#[test]`
//!
//! Verified directly while writing this file: `gtk::init()`'s success is
//! decided entirely by GDK's backend/display auto-probe, which only ever
//! reads it from the process environment (`GDK_BACKEND`, `BROADWAY_DISPLAY`
//! — the same two `tests/headless_smoke.rs` sets on the `hop-gtk`
//! subprocess it spawns). `gdk::set_allowed_backends("broadway")` plus an
//! explicit `gdk::Display::open(Some(":N"))` can open a broadway connection
//! by function argument alone, with no environment variable involved — but
//! `gtk::init()` itself still refuses with "Failed to initialize GTK"
//! unless its own default-display resolution (env-only) succeeds, and once
//! it has refused, every widget constructor afterward panics with "GTK has
//! not been initialized", regardless of a display opened by hand
//! afterward. So `GDK_BACKEND`/`BROADWAY_DISPLAY` have to be set in *this*
//! process's own environment before `gtk::init()` runs — and the only sound
//! way to do that here is on a child process's environment:
//! `std::process::Command::env` sets a *child's* environment and needs no
//! `unsafe`, whereas mutating this process's own environment in place would
//! need `std::env::set_var`, which is an `unsafe fn` on this toolchain, and
//! this crate denies `unsafe_code` — including in tests, per this issue's
//! brief ("No new `unsafe`"). The `#[test]` function below re-execs
//! [`std::env::current_exe`] (this same test binary) with those two
//! variables set on the child via `Command::env`, filtered with `--exact`
//! down to just this one test and a marker variable so the child recognizes
//! it should run [`run_assertions`] directly instead of re-execing a
//! second time; the child's exit status becomes this test's own pass/fail.
#![allow(clippy::unwrap_used)]

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib};

use hop_gtk::keymap::{Action as KeymapAction, Keymap};
use hop_gtk::tokens;
use hop_gtk::ui::row;
use hop_gtk::ui::view::{self, Node};
use hop_protocol::{
    Action, ActionId, ActionKind, IconName, IconPath, IconSpec, Item, ItemId, ItemSubtitle,
    ItemTitle, Kind,
};

/// Set on the re-exec'd child so it knows to run [`run_assertions`]
/// in-process instead of spawning a second child — see this file's module
/// doc.
const CHILD_MARKER: &str = "HOP_GTK_VIEW_TREE_TEST_CHILD";

/// A spawned `gtk4-broadwayd`, killed on drop — the same shape as
/// `tests/headless_smoke.rs`'s own `BroadwayServer`, duplicated rather than
/// shared for the same reason that file's `DaemonProcess` duplicates
/// `crates/hopd/tests/socket.rs`'s helper: each integration test file under
/// `tests/` compiles as its own separate crate, with no shared module
/// unless routed through `tests/common`, and this is the only piece this
/// file needs from it. Display number is derived from this process's own
/// pid, exactly as in `headless_smoke.rs`, so parallel `cargo test`
/// invocations of *this* file and of `headless_smoke.rs` do not collide on
/// the same broadway display.
struct BroadwayServer {
    child: Child,
    display: u32,
}

impl BroadwayServer {
    fn start() -> Self {
        let display = 200 + (std::process::id() % 5000);
        let child = Command::new("gtk4-broadwayd")
            .arg(format!(":{display}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect(
                "failed to spawn gtk4-broadwayd — it ships in libgtk-4-bin \
                 (NOT `broadwayd` on $PATH, which on Debian/Ubuntu is \
                 libgtk-3-bin's incompatible GTK3 server; see \
                 headless_smoke.rs's top doc comment for how this was \
                 diagnosed)",
            );
        // Asynchronous socket creation — see `headless_smoke.rs`'s
        // `BroadwayServer::start` for why this is a fixed sleep rather than
        // a `Path::exists` poll (the socket lives in the abstract
        // namespace).
        std::thread::sleep(Duration::from_millis(300));
        BroadwayServer { child, display }
    }
}

impl Drop for BroadwayServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn setup_builds_a_dispatch_container_and_bind_recycles_the_row_widget() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        run_assertions();
        return;
    }

    let broadway = BroadwayServer::start();

    // Stands in for the child's `$XDG_DATA_HOME`, so its own
    // `icon_roots::from_env` computation includes `<this>/icons` as a real,
    // startup-derived allowed root — see this file's module doc for why
    // this exists and why it must be held alive (below) for the whole of
    // the child's lifetime rather than dropped once `Command::env` has read
    // its path.
    let xdg_data_home = tempfile::tempdir()
        .expect("failed to create a tempdir to stand in for the child's XDG_DATA_HOME");

    let output = Command::new(std::env::current_exe().unwrap())
        .env("GDK_BACKEND", "broadway")
        .env("BROADWAY_DISPLAY", format!(":{}", broadway.display))
        .env(CHILD_MARKER, "1")
        .env("XDG_DATA_HOME", xdg_data_home.path())
        .arg("--exact")
        .arg("setup_builds_a_dispatch_container_and_bind_recycles_the_row_widget")
        .arg("--nocapture")
        .output()
        .expect("failed to re-exec this test binary under the headless broadway display");

    assert!(
        output.status.success(),
        "the headless child process failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// The real assertions, run inside the re-exec'd child process described in
/// this file's module doc, once `GDK_BACKEND=broadway` and
/// `BROADWAY_DISPLAY` are already set in its environment.
fn run_assertions() {
    gtk::init().expect("gtk init under the broadway display this process's environment selects");

    // The allowed icon root this process's own `icon_roots::from_env`
    // computation derives from `$XDG_DATA_HOME` (set by the parent process
    // above, on this child's environment, before it was spawned) — created
    // here, inside the child, rather than by the parent, since it is this
    // process's own `icon_roots::icon_roots` that decides the exact
    // subdirectory name (`icons`) a root built this way has to have.
    let xdg_data_home = std::env::var("XDG_DATA_HOME").expect(
        "the parent process must set XDG_DATA_HOME on this child for issue #93's icon \
         allow-list coverage below to have a real, startup-derived root to write fixtures into",
    );
    let icon_root = std::path::PathBuf::from(&xdg_data_home).join("icons");
    std::fs::create_dir_all(&icon_root)
        .expect("failed to create the allowed icon root this child was handed");

    // The §8 default keymap, and — issue #197 review, finding 3 — its
    // `Activate` binding's display string, resolved exactly once here
    // rather than per `Node::for_item` call, exactly as
    // `ui::view::build` now does (see that function's own doc comment).
    // Every `Node::for_item` call below threads a clone of the small
    // `Option<String>` this produces, not the `Keymap` itself, matching
    // what `ui::view::bind`/`ui::view::unbind` actually receive from a
    // real factory. One shared value, built once, rather than a fresh
    // `Keymap::defaults()` at every call site: this file's own "issue
    // #197" sections below rely on `activate_key_display` answering the
    // same binding's display text (`"Enter"`, per §8's `Return` default)
    // throughout, and a single `let` makes that an invariant of the
    // test's own setup rather than something every assertion has to trust
    // independently.
    let keymap = Keymap::defaults();
    let activate_key_display = keymap.activate_binding_display();

    // --- brief test 1: the slot's child after setup is the dispatch
    // container, not a bare label. Driven through the real factory
    // `ui::view::build` returns — the same one `ui::window::HopWindow`
    // wires into its `GtkListView` — and a manufactured `gtk::ListItem`
    // (GTK exposes no public constructor for the item a real list view
    // would hand `setup`, but `glib::Object::new` builds one all the same,
    // since GTK does not mark the type non-instantiable — only its `item`
    // property, bound internally by a real list view, is read-only). The
    // "setup" signal `connect_setup` installs a handler for is a real named
    // GObject signal (`SignalListItemFactory`'s `connect_setup` returns a
    // `SignalHandlerId`, which only backs an actual `g_signal_connect`), so
    // emitting it by hand exercises the exact closure production wires in,
    // not a copy of its logic.
    let factory = view::build(keymap.clone());
    let list_item: gtk::ListItem = glib::Object::new();
    factory.emit_by_name::<()>("setup", &[&list_item]);

    let stack = list_item
        .child()
        .and_then(|widget| widget.downcast::<gtk::Stack>().ok())
        .expect(
            "setup must give the slot a gtk::Stack dispatch container, not a bare label — the \
             shape ui/row.rs built directly before this issue's seam",
        );

    // --- brief tests 2-4, at the level D4 of the plan names as the right
    // one: `ui::view::bind`/`unbind` directly against the stack `setup`
    // already built, not through a second real `gtk::ListItem` — a real
    // list item's `item` property has no public setter (GTK gives it one
    // only via a bound `GtkListView`), so there is no way to manufacture
    // "a list item bound to item X" from application code the way this
    // test could manufacture the item above. The dispatch function's own
    // signature is what makes that unreachable path a non-issue: `bind`
    // takes `&gtk::Stack`, not `&gtk::ListItem`, so there is no slot-level
    // `set_child` in scope for it to reach for even by mistake — see
    // `ui::view::bind`'s doc comment for why that shape was chosen deliberately.
    let item_a = test_item(1, "first result");
    let item_b = test_item(2, "second result");

    // Every `Node` built below goes through `Node::for_item` rather than
    // naming `Node::Row` directly — the same call `view::build()`'s
    // `connect_bind`/`connect_unbind` closures make after review's second
    // finding on this issue: an earlier version of those closures wrote
    // `Node::Row(item)` inline, which put the "which variant does this
    // item become" decision inside the factory itself, a change to the
    // factory's own structure a second node type would have had
    // to duplicate into both closures to extend — exactly what acceptance
    // criterion 3 rules out. `Node::for_item` unconditionally returns
    // `Node::Row(item)` today, so no runtime assertion can tell its output
    // apart from constructing `Node::Row` by hand — that part of the fix
    // is a code-shape property, checked by reading `view.rs`'s `build`,
    // not by a test (see `Node::for_item`'s own doc comment, and criterion
    // 3's original text: "demonstrated by a test or by the shape of the
    // code"). What this test *can* still pin is that the constructor is a
    // real, working entry point — calling it drives the exact same
    // dispatch, recycling, and rendering behavior a hand-built `Node::Row`
    // would, through every assertion below.
    view::bind(
        &stack,
        &Node::for_item(item_a.clone(), activate_key_display.clone()),
    );
    assert_eq!(
        stack.visible_child_name().as_deref(),
        Some("row"),
        "bind must select the Row page by name, not replace the slot's child"
    );
    let widget_after_first_bind = stack
        .visible_child()
        .expect("a page must be the stack's visible child once bind has run");
    let container_after_first_bind = widget_after_first_bind
        .downcast_ref::<gtk::Box>()
        .expect("the Row page's widget is the gtk::Box ui/row.rs builds")
        .clone();
    let label_after_first_bind = row::title_widget(&container_after_first_bind)
        .expect("build must give the row a named title label");
    assert_eq!(
        label_after_first_bind.text(),
        item_a.title.as_str(),
        "the rendered row must show the bound item's title"
    );

    view::bind(
        &stack,
        &Node::for_item(item_b.clone(), activate_key_display.clone()),
    );
    let widget_after_second_bind = stack
        .visible_child()
        .expect("a page must still be the stack's visible child after the second bind");
    assert_eq!(
        widget_after_second_bind, widget_after_first_bind,
        "recycling: binding a slot to a second item must reuse the same widget instance, \
         never destroy and rebuild it — acceptance criterion 5"
    );
    assert_eq!(
        row::title_widget(&container_after_first_bind),
        Some(label_after_first_bind.clone()),
        "recycling must hold for the title label too: still the exact same gtk::Label \
         instance after a second bind, not a fresh one built by find_named_child returning a \
         different widget"
    );
    assert_eq!(
        label_after_first_bind.text(),
        item_b.title.as_str(),
        "the same recycled label must now show the second item's title"
    );

    // Pins the fix that came out of review: `unbind` takes the `&Node` it
    // is clearing, dispatching on it exactly like `bind` does, rather than
    // reaching for a hardcoded page name — the earlier shape assumed GTK's
    // `unbind` signal carries no item to build a `Node` from, which
    // `ui::view::unbind`'s doc comment now documents as having been wrong
    // (checked against GTK's own `SignalListItemFactory::unbind`
    // documentation). Passing `item_b` here — the item most recently
    // bound, exactly what `list_item.item()` would still return inside a
    // real `connect_unbind` handler at this point — is what this test can
    // do to stand in for that handler's own call.
    view::unbind(
        &stack,
        &Node::for_item(item_b.clone(), activate_key_display.clone()),
    );
    assert_eq!(
        label_after_first_bind.text(),
        "",
        "unbind must clear the row's text, exactly as ui/row.rs's connect_unbind did before \
         this refactor"
    );

    println!("view-tree dispatch and recycling assertions passed");

    // --- issue #190: the icon slot. Everything below reuses the exact same
    // `stack`/slot the assertions above already bound twice and unbound
    // once, so every assertion in this section is also, incidentally,
    // another round of the same recycling proof: if any bind below ever
    // got a fresh widget instead of the one `setup` built once, the
    // `container` identity assertion at the top of this section would
    // catch it before any of the icon-specific assertions below even run.
    let container = stack
        .visible_child()
        .and_then(|w| w.downcast::<gtk::Box>().ok())
        .expect("the Row page's widget is still the same gtk::Box after the section above");
    assert_eq!(
        container, container_after_first_bind,
        "the icon-slot assertions below must run against the exact same recycled row widget \
         the title assertions above just proved recycling holds for"
    );
    let icon = row::icon_widget(&container).expect("build must give the row a named icon image");

    // The row's reserved layout, captured once with no icon bound at all
    // (item_a carries `icon: None`) and asserted unchanged after every
    // subsequent case below. `measure`'s `natural` component is what a
    // real `GtkBox`/`GtkStack` parent actually allocates from — unlike the
    // `height_request`/`width_request` property getters, which only ever
    // echo back whatever was last poked at the widget and would keep
    // reporting the old value even if some future change made the widget
    // *ignore* its own request. A regression that let, say, a large
    // decoded texture grow the icon past its reserved size, or let
    // `resolve_icon` fail to reserve space for a cleared icon, would move
    // one of these three numbers — that is what makes this assertion one
    // that actually fails on a layout shift rather than one that could
    // pass by construction.
    view::bind(
        &stack,
        &Node::for_item(item_a.clone(), activate_key_display.clone()),
    );
    let baseline_layout = row_layout(&container, &icon);
    assert_eq!(
        baseline_layout,
        (
            *tokens::ROW_HEIGHT_PX,
            *tokens::ICON_SIZE_PX,
            *tokens::ICON_SIZE_PX
        ),
        "the row and icon slot must be reserved at exactly the tokens.css sizes with no icon \
         bound at all"
    );

    // `IconSpec::Name`, a name the Adwaita theme installed in this test's
    // environment does have (`/usr/share/icons/Adwaita/scalable/places/folder.svg`).
    // `ui::row::resolve_icon` hands this straight to
    // `gtk::Image::set_icon_name` and does nothing else, so what is
    // checked here is that pass-through: the image's storage type becomes
    // `IconName` and its `icon-name` property is exactly the name given.
    let folder_item = item_with_icon(
        5,
        "has a real icon name",
        IconSpec::Name(icon_name("folder")),
    );
    view::bind(
        &stack,
        &Node::for_item(folder_item.clone(), activate_key_display.clone()),
    );
    assert_eq!(icon.storage_type(), gtk::ImageType::IconName);
    assert_eq!(icon.icon_name().as_deref(), Some("folder"));
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "a resolved icon-name must not change the row's reserved layout"
    );

    // `IconSpec::Name`, a name this environment's theme does *not* have.
    // `resolve_icon`'s brief is explicit that this arm does not
    // special-case a lookup miss — it sets the same property the `folder`
    // case above does and trusts GTK's own icon-name rendering to fall
    // back to `image-missing` on its own. That fallback is verified here
    // directly against the same `gtk::IconTheme` GTK's own icon-name
    // rendering path consults (`GtkIconTheme::lookup_icon`, the same
    // lookup a `gtk::Image` showing an icon-name performs internally to
    // paint itself): looking up this exact name comes back with an
    // `IconPaintable` whose own `icon-name` is `image-missing`, which is
    // precisely GTK's documented "none of the given icon names were
    // found" behavior.
    let missing_name = "hop-test-icon-the-adwaita-theme-does-not-ship";
    let display = gdk::Display::default().expect("a broadway display must be open by this point");
    let theme = gtk::IconTheme::for_display(&display);
    assert!(
        !theme.has_icon(missing_name),
        "test bug: {missing_name} must not actually exist in this environment's icon theme, or \
         this test proves nothing about the lookup-miss path"
    );
    let missing_name_item = item_with_icon(
        6,
        "name the theme lacks",
        IconSpec::Name(icon_name(missing_name)),
    );
    view::bind(
        &stack,
        &Node::for_item(missing_name_item, activate_key_display.clone()),
    );
    assert_eq!(icon.storage_type(), gtk::ImageType::IconName);
    assert_eq!(
        icon.icon_name().as_deref(),
        Some(missing_name),
        "resolve_icon's Name arm must pass the name through unchanged, not try to detect the \
         miss itself — GTK's own rendering is what falls back, per this issue's brief"
    );
    let looked_up = theme.lookup_icon(
        missing_name,
        &[],
        *tokens::ICON_SIZE_PX,
        1,
        gtk::TextDirection::Ltr,
        gtk::IconLookupFlags::empty(),
    );
    assert_eq!(
        looked_up.icon_name().as_deref(),
        Some(std::path::Path::new("image-missing")),
        "GTK's own icon-name resolution for a name the theme lacks must be the image-missing \
         fallback — the desired behavior this issue's brief names explicitly"
    );
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "a theme miss must not change the row's reserved layout either"
    );

    // `IconSpec::Path`, a real, decodable image file written under
    // `icon_root` — an allowed icon root by construction (this file's
    // module doc explains why it has to be, since issue #93) — and opened
    // through `IconPath::open_regular_file` — the one opener issue #190's
    // agent brief names as this crate's sole allowed way to open an icon
    // file ("no second opener is introduced anywhere in the crate"); see
    // `load_path_texture`'s own doc comment in row.rs for why that
    // restriction exists, and for issue #93's allow-list check this now
    // also has to pass. `LARGE_ICON_PNG` is a valid, decodable 256x256
    // PNG, replacing a 1x1 pixel this test used before review — see its own
    // doc comment for what that change does and does not buy this
    // assertion — so `load_path_texture` inside `resolve_icon` should reach
    // its success path: a decoded `gdk::Texture` set as the image's
    // paintable.
    let real_icon_path = icon_root.join("icon.png");
    std::fs::write(&real_icon_path, LARGE_ICON_PNG).expect("failed to write the test icon file");
    let path_item = item_with_icon(
        7,
        "has a real icon file",
        IconSpec::Path(icon_path(&real_icon_path)),
    );
    view::bind(
        &stack,
        &Node::for_item(path_item, activate_key_display.clone()),
    );
    assert_eq!(icon.storage_type(), gtk::ImageType::Paintable);
    assert!(
        icon.paintable().is_some(),
        "a decoded icon file must be set as the image's paintable"
    );
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "a resolved icon file must not change the row's reserved layout — held here by \
         ui::row::build's unconditional icon.set_size_request(ICON_SIZE_PX, ICON_SIZE_PX), not \
         by this image's size (see LARGE_ICON_PNG's doc comment)"
    );

    // `IconSpec::Path` pointing at a directory: `open_regular_file` refuses
    // to hand back a file at all (`IconOpenError::NotARegularFile`), which
    // is the "the open refused" failure this issue's brief names. `bind`
    // must set `image-missing` explicitly here — unlike the `Name` miss
    // case above, nothing about GTK's own rendering does this for a
    // `Paintable`-storage image, so `resolve_icon` has to do it itself.
    // `icon_root` itself stands in for the directory: it is a real
    // directory under an allowed root, and this case is about
    // `open_regular_file`'s own regular-file check refusing it before
    // issue #93's allow-list check would ever run, so which directory is
    // used does not matter to what this assertion proves.
    let dir_item = item_with_icon(
        8,
        "path is a directory",
        IconSpec::Path(icon_path(&icon_root)),
    );
    view::bind(
        &stack,
        &Node::for_item(dir_item, activate_key_display.clone()),
    );
    assert_eq!(icon.storage_type(), gtk::ImageType::IconName);
    assert_eq!(icon.icon_name().as_deref(), Some("image-missing"));
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "an open failure must not change the row's reserved layout"
    );

    // `IconSpec::Path` pointing at a real, regular file whose bytes do not
    // decode as any image format `gdk::Texture::from_bytes` understands —
    // the "the bytes did not decode" failure this issue's brief names,
    // distinct from the directory case above (that one never gets past
    // `open_regular_file` at all). Must land on the same `image-missing`
    // outcome.
    let garbage_path = icon_root.join("not-an-image.bin");
    std::fs::write(&garbage_path, b"this is not image data")
        .expect("failed to write the garbage test file");
    let garbage_item = item_with_icon(
        9,
        "path decodes to nothing",
        IconSpec::Path(icon_path(&garbage_path)),
    );
    view::bind(
        &stack,
        &Node::for_item(garbage_item, activate_key_display.clone()),
    );
    assert_eq!(icon.storage_type(), gtk::ImageType::IconName);
    assert_eq!(icon.icon_name().as_deref(), Some("image-missing"));
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "a decode failure must not change the row's reserved layout"
    );

    // --- issue #93: the icon allow-list `ui::row::load_path_texture` now
    // enforces on top of everything the section above already covers. Every
    // fixture below sits under, or deliberately outside, `icon_root` — the
    // allowed root this file's module doc explains how the parent process
    // hands the child via `XDG_DATA_HOME`.

    // A path that is a real, decodable image and would have rendered under
    // the pre-#93 behavior, but sits in a plain tempdir this process's
    // allow-list has no reason to trust: nothing designates an arbitrary
    // directory an icon root just because a provider's `Icon=` line points
    // at a file inside it. Must be refused, not opened.
    let outside_dir =
        tempfile::tempdir().expect("failed to create a tempdir outside every allowed icon root");
    let outside_icon_path = outside_dir.path().join("icon.png");
    std::fs::write(&outside_icon_path, LARGE_ICON_PNG)
        .expect("failed to write the outside-root test icon file");
    let outside_item = item_with_icon(
        11,
        "path resolves outside every allowed icon root",
        IconSpec::Path(icon_path(&outside_icon_path)),
    );
    view::bind(
        &stack,
        &Node::for_item(outside_item, activate_key_display.clone()),
    );
    assert_eq!(icon.storage_type(), gtk::ImageType::IconName);
    assert_eq!(
        icon.icon_name().as_deref(),
        Some("image-missing"),
        "a path resolving outside every allowed icon root must not be opened — issue #93"
    );
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "an allow-list refusal must not change the row's reserved layout"
    );

    // A symlink that sits *under* an allowed root (`icon_root` itself) but
    // whose target is the same outside-root file used above. Textually the
    // path passed on the wire (`icon_root.join("escapes.png")`) looks
    // exactly as trustworthy as `real_icon_path` above; only resolving it
    // — which is the entire difficulty this issue's brief calls out — shows
    // the difference. Must be refused, exactly as `outside_item` was,
    // pinning that a symlink cannot be used to launder an outside-root
    // path through an allowed directory.
    let escaping_link = icon_root.join("escapes.png");
    std::os::unix::fs::symlink(&outside_icon_path, &escaping_link)
        .expect("failed to create the root-escaping symlink fixture");
    let escaping_item = item_with_icon(
        12,
        "symlink under an allowed root leads outside every one",
        IconSpec::Path(icon_path(&escaping_link)),
    );
    view::bind(
        &stack,
        &Node::for_item(escaping_item, activate_key_display.clone()),
    );
    assert_eq!(icon.storage_type(), gtk::ImageType::IconName);
    assert_eq!(
        icon.icon_name().as_deref(),
        Some("image-missing"),
        "a symlink under an allowed root that resolves outside every one must not be opened — \
         issue #93, and the case a bare textual prefix check on the path string would have \
         missed"
    );
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "an allow-list refusal via a symlink must not change the row's reserved layout"
    );

    // An ordinary symlink that both lives under an allowed root *and*
    // resolves to a target still under that same root — the shape a real
    // icon theme's own symlinks take (`/usr/share/icons/hicolor` is largely
    // links between sizes and themes). This is the acceptance criterion
    // that rules out `O_NOFOLLOW`: refusing to follow a symlink at all
    // would refuse this one too, even though nothing about it reaches
    // outside the allow-list. Must still open and decode, exactly like
    // `path_item` above.
    let themed_target = icon_root.join("themed-real.png");
    std::fs::write(&themed_target, LARGE_ICON_PNG)
        .expect("failed to write the themed-icon symlink target fixture");
    let themed_link = icon_root.join("themed-link.png");
    std::os::unix::fs::symlink(&themed_target, &themed_link)
        .expect("failed to create the in-root themed-icon symlink fixture");
    let themed_item = item_with_icon(
        13,
        "ordinary themed icon reached through a symlink",
        IconSpec::Path(icon_path(&themed_link)),
    );
    view::bind(
        &stack,
        &Node::for_item(themed_item, activate_key_display.clone()),
    );
    assert_eq!(icon.storage_type(), gtk::ImageType::Paintable);
    assert!(
        icon.paintable().is_some(),
        "an ordinary symlink within an allowed root must still open and decode — the case that \
         makes O_NOFOLLOW the wrong tool here, issue #93"
    );
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "a resolved icon reached through an in-root symlink must not change the row's reserved \
         layout"
    );

    // `/proc/self/mem`, named literally per issue #93's acceptance
    // criteria: the regular file the issue exists to close a path to. It
    // passes every rule `IconPath::new` applies (absolute, no `..`
    // component, no NUL, no control character) and `open_regular_file`
    // opens it successfully (it really is a regular file by its mode), so
    // only the allow-list check added by this issue stands between it and
    // the decoder.
    let procfs_item = item_with_icon(
        14,
        "proc self mem",
        IconSpec::Path(icon_path(std::path::Path::new("/proc/self/mem"))),
    );
    view::bind(
        &stack,
        &Node::for_item(procfs_item, activate_key_display.clone()),
    );
    assert_eq!(icon.storage_type(), gtk::ImageType::IconName);
    assert_eq!(
        icon.icon_name().as_deref(),
        Some("image-missing"),
        "/proc/self/mem must never be opened — issue #93"
    );
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "refusing /proc/self/mem must not change the row's reserved layout"
    );

    println!("row icon allow-list assertions passed");

    // Rebinding the same slot to `icon: None`, right after it held a
    // resolved icon (the garbage-path bind just above still shows
    // `image-missing`, which is itself an icon-name being displayed) — the
    // brief's own recycling requirement: "a row bound to an item with an
    // icon then rebound to `icon: None` shows no leftover icon." `Empty`
    // is `gtk::Image`'s own "nothing is set" storage type, distinct from
    // both `IconName` (even the `image-missing` case above) and
    // `Paintable`, so this is the assertion that would fail if `clear()`
    // were ever dropped from `resolve_icon`'s `None` arm and the previous
    // bind's icon silently kept showing.
    let blank_item = item_with_icon_none(10, "icon removed on rebind");
    view::bind(
        &stack,
        &Node::for_item(blank_item, activate_key_display.clone()),
    );
    assert_eq!(
        icon.storage_type(),
        gtk::ImageType::Empty,
        "rebinding to icon: None must leave no leftover icon from the previous bind"
    );
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "clearing the icon back to blank must not change the row's reserved layout"
    );

    // `unbind`'s own symmetry: it must clear the icon exactly as it clears
    // the title, so a recycled row about to be rebound to a *different*
    // node type someday would not carry this row's icon forward by
    // accident. Rebind to an item with a real icon first so there is
    // something for `unbind` to actually clear.
    view::bind(
        &stack,
        &Node::for_item(folder_item, activate_key_display.clone()),
    );
    assert_eq!(icon.storage_type(), gtk::ImageType::IconName);
    view::unbind(
        &stack,
        &Node::for_item(folder_item_for_unbind(), activate_key_display.clone()),
    );
    assert_eq!(
        icon.storage_type(),
        gtk::ImageType::Empty,
        "unbind must clear the icon, symmetrically with the title it already clears"
    );

    println!("row icon assertions passed");

    // --- issue #196: the subtitle. Continues binding the exact same
    // recycled slot the sections above already proved recycling for, so
    // every assertion below is also, incidentally, one more round of that
    // same proof.

    // The structural claim behind "the subtitle renders beneath the title,
    // not beside it": title and subtitle must share one parent — the
    // vertical text-column `gtk::Box` `ui::row::build` nests inside the
    // outer horizontal container — and that parent must be a *different*
    // widget than the icon's own parent (the outer container itself). A
    // regression that flattened the layout back to one `append` per widget
    // onto `container` would give the subtitle the icon's exact parent,
    // which the first assertion below would catch; a regression that used
    // a horizontal box for the nested column would fail the second.
    let title = row::title_widget(&container).expect("title widget must still resolve");
    let subtitle =
        row::subtitle_widget(&container).expect("build must give the row a named subtitle label");
    let title_parent = title.parent().expect("title must have a parent widget");
    let subtitle_parent = subtitle
        .parent()
        .expect("subtitle must have a parent widget");
    assert_eq!(
        title_parent, subtitle_parent,
        "title and subtitle must share one parent — the vertical text column"
    );
    let container_widget = container.clone().upcast::<gtk::Widget>();
    assert_ne!(
        title_parent, container_widget,
        "the text column must be a distinct nested Box, not the outer row container itself \
         the icon is a direct child of"
    );
    let text_column = title_parent
        .downcast::<gtk::Box>()
        .expect("the text column must itself be a gtk::Box");
    assert_eq!(
        text_column.orientation(),
        gtk::Orientation::Vertical,
        "the text column must stack its two children vertically — title over subtitle, not \
         beside it"
    );
    assert_eq!(
        text_column.parent(),
        Some(container_widget),
        "the text column itself must be the outer row container's second child, alongside the \
         icon"
    );

    // `item_a` (bound at the very top of the icon section above) carries
    // `subtitle: None` — this is the first assertion this file makes about
    // what that bind actually did to the subtitle widget. See
    // `ui::row::bind`'s own doc comment for the deliberately-chosen rule
    // this pins: an absent subtitle hides the widget entirely rather than
    // leaving it visible with empty text, which would sit the title above
    // a blank gap instead of letting it recover the row's full height.
    assert!(
        !subtitle.is_visible(),
        "a row bound to an item with subtitle: None must hide the subtitle widget entirely, \
         not just clear its text"
    );
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "a hidden subtitle must not change the row's reserved layout"
    );

    let subtitled_item = item_with_subtitle(20, "has a subtitle", "a real subtitle line");
    view::bind(
        &stack,
        &Node::for_item(subtitled_item.clone(), activate_key_display.clone()),
    );
    assert_eq!(
        subtitle.text(),
        "a real subtitle line",
        "a row bound to an item with Some(subtitle) must show its text"
    );
    assert!(
        subtitle.is_visible(),
        "a row bound to an item with Some(subtitle) must show the subtitle widget"
    );
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "a bound subtitle must not change the row's reserved layout — row height and icon slot \
         stay exactly as ui::row::build reserved them"
    );

    // Recycling, proven specifically across bind-with-subtitle then
    // rebind-without: same widget instance, previous text gone — the
    // acceptance criterion this issue's brief names explicitly.
    view::bind(
        &stack,
        &Node::for_item(item_a.clone(), activate_key_display.clone()),
    );
    assert_eq!(
        row::subtitle_widget(&container).as_ref(),
        Some(&subtitle),
        "rebinding to an item without a subtitle must reuse the exact same subtitle widget \
         instance, never destroy and rebuild it"
    );
    assert_eq!(
        subtitle.text(),
        "",
        "rebinding without a subtitle must clear the previous item's subtitle text"
    );
    assert!(
        !subtitle.is_visible(),
        "rebinding without a subtitle must hide the widget again, not leave the previous \
         item's subtitle visible"
    );
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "rebinding away from a subtitle must not change the row's reserved layout either"
    );

    // `unbind`'s own symmetry with the title and icon: clears the text and
    // hides the widget, so a recycled row about to be rebound to a
    // different node type someday would not carry a stale subtitle
    // forward.
    view::bind(
        &stack,
        &Node::for_item(subtitled_item.clone(), activate_key_display.clone()),
    );
    assert!(subtitle.is_visible());
    view::unbind(
        &stack,
        &Node::for_item(subtitled_item, activate_key_display.clone()),
    );
    assert_eq!(
        subtitle.text(),
        "",
        "unbind must clear the subtitle text, symmetrically with the title and icon"
    );
    assert!(
        !subtitle.is_visible(),
        "unbind must hide the subtitle widget, symmetrically with clearing its text"
    );

    println!("row subtitle assertions passed");

    // --- issue #197: the action hint. Continues binding the exact same
    // recycled slot every section above already proved recycling for.

    // `item_a`'s own default action ("open", per `test_item`) resolves to
    // its `Action`'s label, "Open"; `keymap`'s §8 default binding for
    // `KeymapAction::Activate` is `Return`, which `Binding`'s own
    // `fmt::Display` convention spells "Enter" (`crate::keymap`'s doc
    // comment on that `impl` names the rule). Rebinding to `item_a` here
    // proves both chips render together, paired correctly.
    view::bind(
        &stack,
        &Node::for_item(item_a.clone(), activate_key_display.clone()),
    );
    let hint_label = row::hint_label_widget(&container)
        .expect("build must give the row a named hint label chip");
    let hint_key =
        row::hint_key_widget(&container).expect("build must give the row a named hint key chip");
    let expected_key_text = keymap
        .binding_for(KeymapAction::Activate)
        .expect("the §8 default keymap must answer Activate")
        .to_string();
    assert_eq!(
        hint_label.text(),
        "Open",
        "the hint's label chip must show the item's own default-action label"
    );
    assert!(hint_label.is_visible());
    assert_eq!(
        hint_key.text(),
        expected_key_text,
        "the hint's key chip must show the key that runs keymap::Action::Activate"
    );
    assert!(hint_key.is_visible());
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "a resolved action hint must not change the row's reserved row height or icon slot"
    );

    // The "both halves or neither" rule (`ui::row::resolve_hint`'s own doc
    // comment), from the item side: `default_action` naming an id absent
    // from `actions` entirely — the malformed case
    // `ui::row::default_action_label`'s own doc comment names. Both chips
    // must go empty, not a key glyph left standing with no label (the
    // keymap side of this same rule is not independently reachable through
    // `Keymap`'s own public construction API: `Keymap::defaults` seeds
    // every `keymap::Action` including `Activate`, and a `config.toml`
    // rebinding can only override an action's key, never remove it, so
    // `binding_for(Activate)` cannot actually answer `None` for any
    // `Keymap` this crate can build today — `resolve_hint`'s own `let
    // (Some(label), Some(key)) = (...) else` treats both cases identically
    // regardless).
    let stale_default_action_item = item_with_actions(31, "default action names no real action");
    view::bind(
        &stack,
        &Node::for_item(stale_default_action_item, activate_key_display.clone()),
    );
    assert_eq!(hint_label.text(), "");
    assert!(!hint_label.is_visible());
    assert_eq!(hint_key.text(), "");
    assert!(!hint_key.is_visible());
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "an empty hint slot must not change the row's reserved layout either"
    );

    // `unbind`'s own symmetry with the title, subtitle, and icon: clears
    // and hides both chips, so a recycled row about to be rebound to a
    // different item someday would not carry a stale hint forward.
    view::bind(
        &stack,
        &Node::for_item(item_a.clone(), activate_key_display.clone()),
    );
    assert!(hint_label.is_visible());
    assert!(hint_key.is_visible());
    view::unbind(
        &stack,
        &Node::for_item(item_a.clone(), activate_key_display.clone()),
    );
    assert_eq!(
        hint_label.text(),
        "",
        "unbind must clear the hint label text, symmetrically with the title, subtitle, and icon"
    );
    assert!(!hint_label.is_visible());
    assert_eq!(hint_key.text(), "");
    assert!(!hint_key.is_visible());

    println!("row action hint assertions passed");

    // --- issue #207: the hint's entrance fade never replays on a bare
    // recycle. Continues on the exact same recycled slot every section
    // above already proved recycling for. This is the "observable state,
    // not animation timing or pixels" proof the recycling constraint
    // needs — `ui::row::HINT_SHOWN_CLASS`'s presence is what
    // `assets/stylesheet.css`'s `.hop-row-hint.hop-row-hint-shown` rule
    // matches on to actually play the entrance fade, and `ui::row::bind`'s
    // own doc comment ("the recycling constraint") is the mechanism this
    // section drives directly. `ui::row::tests::hint_entered_shown_*`/
    // `hint_left_shown_*` (this crate's own `src/ui/row.rs`) already prove
    // the pure decision table with no GTK at all; what only a real
    // `gtk::Box` can prove is that `bind`/`unbind` actually wire that
    // decision to a real widget's real CSS class, which is what this
    // section does.
    let hint = row::hint_widget(&container).expect("build must give the row a named hint wrapper");

    // A known starting point, driven rather than assumed: bind an item
    // with no hint at all first, so `HINT_SHOWN_CLASS` is genuinely absent
    // going in regardless of this recycled row's own binding history
    // above (every item bound in the icon and subtitle sections carries a
    // real hint via `test_item`, so the class may already have been
    // present long before this section ever ran).
    let starts_with_no_hint = item_with_actions(40, "no hint to start this section from");
    view::bind(
        &stack,
        &Node::for_item(starts_with_no_hint, activate_key_display.clone()),
    );
    assert!(
        !hint.has_css_class(row::HINT_SHOWN_CLASS),
        "starting baseline: the hint must not carry HINT_SHOWN_CLASS while genuinely un-shown"
    );

    // A genuine not-shown-to-shown transition: bind to an item with a real
    // default action. This is the one case that adds the class.
    view::bind(
        &stack,
        &Node::for_item(item_a.clone(), activate_key_display.clone()),
    );
    assert!(
        hint.has_css_class(row::HINT_SHOWN_CLASS),
        "a genuine not-shown-to-shown transition must add HINT_SHOWN_CLASS — this is what \
         makes assets/stylesheet.css's .hop-row-hint.hop-row-hint-shown rule match and play \
         the entrance fade"
    );
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "adding the shown class must not change the row's reserved row height or icon slot — \
         opacity is a paint property, not a layout one"
    );

    // GTK calls `unbind` before every `bind` that reassigns a recycled
    // slot's item (`ui::view::unbind`'s own doc comment) — this stands in
    // for that. It must clear the chips' own visibility (already proven
    // above, "row action hint assertions") but, critically, never the
    // class — see `ui::row::unbind`'s own doc comment, "the one deliberate
    // exception to that symmetry."
    view::unbind(
        &stack,
        &Node::for_item(item_a.clone(), activate_key_display.clone()),
    );
    assert!(
        hint.has_css_class(row::HINT_SHOWN_CLASS),
        "unbind must never remove HINT_SHOWN_CLASS — it is this widget's own persistent \
         memory of \"was the hint genuinely showing,\" and it has to survive unbind untouched \
         so the next bind can tell a bare recycle apart from a genuine entrance"
    );

    // The recycle itself: rebind to a *different* item whose hint also
    // resolves, but to different text ("Copy", not "Open") — proving the
    // class tracks the hint's shown/hidden *state*, never its text, and
    // therefore must not replay the fade here: the class was already
    // present and must simply stay present, never observably removed and
    // re-added.
    let recycled_with_different_hint_text =
        item_with_default_action_label(41, "second item, different hint text", "Copy");
    view::bind(
        &stack,
        &Node::for_item(
            recycled_with_different_hint_text,
            activate_key_display.clone(),
        ),
    );
    assert!(
        hint.has_css_class(row::HINT_SHOWN_CLASS),
        "a recycled row rebinding to a new item while the hint stays shown throughout must \
         not replay the fade — the class must simply remain present, regardless of whether \
         the new item's hint text differs from the old one's"
    );
    assert_eq!(
        hint_label.text(),
        "Copy",
        "the recycled label must show the second item's own hint text — proving this really \
         was a different item bound to the same widget, not the same item bound twice"
    );

    // The hint genuinely leaving: rebind to an item with no default action
    // match at all. This is the one case besides the initial entrance that
    // changes the class — removing it this time.
    let hint_genuinely_leaves = item_with_actions(42, "hint genuinely leaves on this bind");
    view::bind(
        &stack,
        &Node::for_item(hint_genuinely_leaves, activate_key_display.clone()),
    );
    assert!(
        !hint.has_css_class(row::HINT_SHOWN_CLASS),
        "a genuine shown-to-not-shown transition must remove HINT_SHOWN_CLASS"
    );
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "removing the shown class must not change the row's reserved row height or icon slot \
         either"
    );

    println!("row hint entrance-fade recycling assertions passed (issue #207)");

    // --- issue #254: per-row action icons. Continues on the exact same
    // recycled `stack`/`container` every section above already proved
    // recycling for — no real window is needed here (unlike the
    // responsive-collapse section below): `ui::row::resolve_action_icons`
    // makes no width-driven decision, only a fixed-count one, so a bare
    // `gtk::Box` never added to a window answers exactly like a realized
    // one would.

    // A resting bind — `item_a` carries exactly one action (`test_item`'s
    // own "open") — must show slot 0 and hide slot 1: one real action
    // never means two visible icons.
    view::bind(
        &stack,
        &Node::for_item(item_a.clone(), activate_key_display.clone()),
    );
    let action_icon_1 = row::action_icon_widget(&container, 0)
        .expect("build must give the row a named first action-icon button");
    let action_icon_2 = row::action_icon_widget(&container, 1)
        .expect("build must give the row a named second action-icon button");
    let overflow_button = row::overflow_button_widget(&container)
        .expect("build must give the row a named overflow chevron button");
    assert!(
        row::action_icon_widget(&container, 2).is_none(),
        "there must be no third action-icon button — ROW_ACTION_ICON_CAP is 2, not \
         item.actions.len()"
    );
    assert!(
        action_icon_1.is_visible(),
        "a real first action must show the row's first action-icon button"
    );
    assert!(
        !action_icon_2.is_visible(),
        "an item with only one action must not show a second action icon"
    );
    assert!(
        !overflow_button.is_visible(),
        "issue #254 review, finding 4: an item with only one action — fewer than \
         ROW_ACTION_ICON_CAP — must not show the overflow chevron; every one of its actions \
         already has a dedicated icon"
    );
    assert_eq!(
        row_layout(&container, &icon),
        baseline_layout,
        "action icons appearing or hiding must not change the row's reserved row height or \
         icon slot"
    );

    // Zero actions — a legitimate wire shape — must hide *both* icons, not
    // merely fail to show a first one.
    let no_actions_item = item_with_no_actions(60, "no actions at all");
    view::bind(
        &stack,
        &Node::for_item(no_actions_item, activate_key_display.clone()),
    );
    assert!(
        !action_icon_1.is_visible(),
        "an item with zero actions must hide the first action icon too"
    );
    assert!(!action_icon_2.is_visible());
    assert!(
        !overflow_button.is_visible(),
        "an item with zero actions has nothing to overflow into a panel either"
    );

    // Two actions of two different kinds — both slots must show, each
    // carrying that exact action's own icon, tooltip, and GAction target,
    // not the other's and not the row's default action.
    let two_actions_item = item_with_two_actions(61, "two different actions");
    view::bind(
        &stack,
        &Node::for_item(two_actions_item.clone(), activate_key_display.clone()),
    );
    assert!(action_icon_1.is_visible());
    assert!(action_icon_2.is_visible());
    assert!(
        !overflow_button.is_visible(),
        "issue #254 review, finding 4: exactly ROW_ACTION_ICON_CAP (2) actions must not show \
         the overflow chevron either — every one of this item's actions already has a \
         dedicated icon, so there is nothing left for the panel to hold that the row does not \
         already offer directly"
    );
    assert_eq!(
        action_icon_1.tooltip_text().as_deref(),
        Some("Open"),
        "the first slot's tooltip must name that exact action's own label"
    );
    assert_eq!(
        action_icon_2.tooltip_text().as_deref(),
        Some("Copy path"),
        "the second slot's tooltip must name the *second* action's label, not the first's"
    );
    assert_eq!(
        action_icon_1.action_name().as_deref(),
        Some("row.run-action"),
        "every action-icon button must invoke the same shared GAction — ui::window::HopWindow \
         is what tells the two apart, by target, not by a different action name per button"
    );
    let target_1 = action_icon_1
        .action_target_value()
        .expect("a visible action-icon button must carry a real action target")
        .get::<(String, String)>()
        .expect("the action target must unpack as an (item_id, action_id) pair of strings");
    assert_eq!(
        target_1,
        (two_actions_item.id.as_str().to_string(), "open".to_string()),
        "the first slot's target must name this item's id and its first action's id"
    );
    let target_2 = action_icon_2
        .action_target_value()
        .expect("a visible action-icon button must carry a real action target")
        .get::<(String, String)>()
        .expect("the action target must unpack as an (item_id, action_id) pair of strings");
    assert_eq!(
        target_2,
        (
            two_actions_item.id.as_str().to_string(),
            "copy-path".to_string()
        ),
        "the second slot's target must name the *second* action's id, not the first's default"
    );

    // Three actions — one more than `ROW_ACTION_ICON_CAP` — must still
    // show only the first two, in wire order; the third is only ever
    // reachable through the ctrl-K/right-click action panel, never a
    // dedicated row icon.
    let three_actions_item = item_with_three_actions(62, "three actions, one over the cap");
    view::bind(
        &stack,
        &Node::for_item(three_actions_item.clone(), activate_key_display.clone()),
    );
    assert_eq!(action_icon_1.tooltip_text().as_deref(), Some("Open"));
    assert_eq!(action_icon_2.tooltip_text().as_deref(), Some("Copy path"));
    assert!(
        overflow_button.is_visible(),
        "issue #254 review, finding 4: an item declaring one more action than \
         ROW_ACTION_ICON_CAP must show the overflow chevron — \"reveal\" has nowhere else to \
         go from this row"
    );
    let overflow_target = overflow_button
        .action_target_value()
        .expect("a visible overflow chevron must carry a real action target")
        .get::<String>()
        .expect("the overflow chevron's action target must unpack as a bare item id string");
    assert_eq!(
        overflow_target,
        three_actions_item.id.as_str(),
        "the overflow chevron's target must name this row's own item, so the window can \
         select and re-present the panel for the same item a click on it opens"
    );
    let target_2_of_three = action_icon_2
        .action_target_value()
        .expect("a visible action-icon button must carry a real action target")
        .get::<(String, String)>()
        .expect("the action target must unpack as an (item_id, action_id) pair of strings");
    assert_eq!(
        target_2_of_three,
        (
            three_actions_item.id.as_str().to_string(),
            "copy-path".to_string()
        ),
        "the third action (\"reveal\") must never reach a row icon at all — the cap truncates, \
         it does not shift which two actions are shown"
    );

    // Recycling: a row shown wide open with two visible icons, rebound to
    // an item with only one action, must not carry the second icon's
    // stale visibility (or its stale target) forward — the exact hazard
    // this module's own doc comment ("the recycling constraint") warns a
    // fixed, unconditional-every-bind rule (rather than a before/after
    // comparison) is what rules out here.
    view::bind(
        &stack,
        &Node::for_item(item_a.clone(), activate_key_display.clone()),
    );
    assert!(
        action_icon_1.is_visible(),
        "recycling back onto a one-action item must still show the first icon"
    );
    assert!(
        !action_icon_2.is_visible(),
        "recycling from a two-action item onto a one-action item must not leave the second \
         action icon visible — a recycled row must not carry a stale icon forward"
    );
    assert!(
        !overflow_button.is_visible(),
        "issue #254 review, finding 4: recycling a row that showed the overflow chevron (three \
         actions) onto an item with only one action must hide it again — the exact \"must not \
         persist onto a rebound row whose item has ≤2 actions\" hazard this review finding \
         names for a per-row affordance with no Rust-side shown/hidden memory of its own"
    );
    assert!(
        overflow_button.action_target_value().is_none(),
        "recycling away from the overflow chevron must clear its stale target too, not merely \
         hide the button"
    );
    let target_after_recycle = action_icon_1
        .action_target_value()
        .expect("a visible action-icon button must carry a real action target")
        .get::<(String, String)>()
        .expect("the action target must unpack as an (item_id, action_id) pair of strings");
    assert_eq!(
        target_after_recycle,
        (item_a.id.as_str().to_string(), "open".to_string()),
        "the first slot's target must be rewritten to the newly-bound item's own action, not \
         left pointing at whichever item most recently used that slot"
    );

    // `unbind`'s own symmetry with the title, subtitle, icon, and hint:
    // every action-icon button must be hidden, its tooltip and target
    // cleared — a recycled row about to be rebound to a different item
    // must not carry stale action data across the gap.
    view::unbind(
        &stack,
        &Node::for_item(item_a.clone(), activate_key_display.clone()),
    );
    assert!(!action_icon_1.is_visible());
    assert!(!action_icon_2.is_visible());
    assert!(
        action_icon_1.action_target_value().is_none(),
        "unbind must clear the action target, not merely hide the button"
    );
    assert!(
        !overflow_button.is_visible(),
        "unbind must hide the overflow chevron too, matching every other optional row element"
    );
    assert!(
        overflow_button.action_target_value().is_none(),
        "unbind must clear the overflow chevron's action target, not merely hide the button"
    );

    // Neither the title nor the subtitle label can be text-selected — SPEC
    // decision 6: "no text selection inside rows (the copy action owns
    // that)."
    let title = row::title_widget(&container).expect("build must give the row a named title");
    let subtitle =
        row::subtitle_widget(&container).expect("build must give the row a named subtitle");
    assert!(
        !title.is_selectable(),
        "the row's title label must not be text-selectable"
    );
    assert!(
        !subtitle.is_selectable(),
        "the row's subtitle label must not be text-selectable"
    );

    println!("row action-icon assertions passed (issue #254)");

    // --- issue #197: the responsive collapse. `assets/tokens.css`'s own
    // GEOMETRY note: "the action hint collapses to icon-only before it
    // would be pushed off-window." GTK has no CSS media query (confirmed
    // against a real GTK 4.14 parser — `assets/stylesheet.css`'s own top
    // doc comment makes the identical finding), so
    // `ui::row::should_show_label_chip`'s collapse can only be proven
    // against a real, mapped top-level window's actual pixel width — never
    // against the disconnected `stack`/`list_item` every section above has
    // driven, which is exactly why every hint assertion above (never added
    // to a window) already exercised `should_show_label_chip`'s own
    // documented "no surface → never collapse" default without this file
    // having said so explicitly until now.
    //
    // This drives `ui::row::build`/`ui::row::bind` directly, deliberately
    // skipping `ui::view`'s dispatch `gtk::Stack`/`gtk::ListItem`
    // apparatus: that machinery is already proven, independently, by every
    // section above, and reusing it here would mean parenting the same
    // `gtk::Stack` under both a bare `gtk::ListItem` (from `setup`) and a
    // real `gtk::Window` — two parents for one widget, which GTK refuses.
    // `row::bind`'s own signature (`&gtk::Widget`, `&Item`, `&Keymap`) asks
    // for nothing `ui::view::bind` does not already have in hand, so
    // calling it directly here proves the identical production code path.
    let (window_w, window_h) = *tokens::WINDOW_SIZE_PX;

    let (wide_window, wide_label, wide_key) =
        realized_hint_row(activate_key_display.as_deref(), &item_a, window_w, window_h);
    assert!(
        wide_label.is_visible(),
        "at the crate's own default window width both hint chips must show"
    );
    assert!(wide_key.is_visible());
    wide_window.close();

    // A genuinely constrained width, not an assumed literal: probed
    // directly against this same real broadway display, before writing the
    // assertion below, by requesting a series of small
    // `gtk::Window::set_default_size` widths for an otherwise-empty window
    // and reading back `gdk::Surface::width()` once mapped. Every request
    // at or under roughly 200px settled at the *same* floor regardless of
    // how small it asked — this GTK/broadway combination refuses to map a
    // toplevel narrower than that, a real environmental constraint this
    // test reproduces rather than a number invented for the assertion.
    // `narrow_width` below asks for less than that floor on purpose, so
    // whatever the environment actually grants is the narrowest real
    // surface this process can produce — the genuine "as constrained as a
    // window can get here" case `should_show_label_chip`'s own doc comment
    // says this issue must be proven against.
    let narrow_width = 80;

    // That floor is comfortably wide enough for `item_a`'s own short
    // "Open"/key-glyph hint to keep showing both chips — which is exactly
    // what the very first assertion in this section already proved is not
    // itself evidence of a working collapse (a hint that never needs more
    // room than the floor grants would show both chips at *any* width,
    // collapsed logic present or not). What actually drives the hint past
    // that floor is a *long* default-action label — the one part of the
    // hint `tokens.css` gives no fixed size at all: an
    // `hop_protocol::item::Action::label` is provider-supplied prose, and a
    // long one is exactly the "before it would be pushed off-window" case
    // `tokens.css`'s GEOMETRY note names.
    let long_label_item = item_with_default_action_label(
        41,
        "an item whose default action has an unusually long label",
        "Copy the full absolute path to the clipboard as plain text",
    );
    let (narrow_window, narrow_label, narrow_key) = realized_hint_row(
        activate_key_display.as_deref(),
        &long_label_item,
        narrow_width,
        window_h,
    );
    assert!(
        !narrow_label.is_visible(),
        "at a real constrained window width, a long enough default-action label must collapse \
         away rather than overflow the row"
    );
    assert!(
        narrow_key.is_visible(),
        "the key glyph pill must remain even once the label chip collapses"
    );
    assert_eq!(narrow_key.text(), expected_key_text);
    narrow_window.close();

    println!("row action hint responsive collapse assertions passed");

    // --- issue #197 code review, finding 1: the collapse threshold must
    // count the hint's own inter-chip gap (`tokens::HINT_CHIP_GAP_PX`) and
    // margin-start (`tokens::HINT_MARGIN_START_PX`) — both always present,
    // together roughly 20px — not just the icon and the two chips' natural
    // widths. See `ui::row::should_show_label_chip`'s own doc comment for
    // the full argument. This section proves two things the sections above
    // do not: (a) a width that satisfies the *old*, undercounting sum but
    // not the corrected one actually collapses, rather than only ever
    // testing widths so narrow any reasonable threshold would collapse
    // there; and (b) the decision is stateless — the exact same target
    // width gives the exact same answer whether the row arrives at it
    // having never collapsed, or having just collapsed and widened back —
    // which rules out an implementation whose answer depends on what the
    // hint currently shows (e.g. measuring the hint container itself,
    // which excludes an already-hidden label chip from its own natural
    // width) rather than only on the width and the item.

    // A moderately long label — long enough that its own natural width, once
    // combined with the icon and the key glyph, clears this environment's
    // real floor width by a comfortable margin (see `narrow_width`'s own
    // comment above for how that floor was found), so every width this
    // section requests below is honored as asked rather than silently
    // clamped up to that floor.
    let gap_margin_item = item_with_default_action_label(
        51,
        "an item whose default action label exercises the gap/margin threshold",
        "Copy the full path to the clipboard as text",
    );

    // The real footprint, measured (not guessed) at a generous width where
    // nothing collapses — the same `gtk::Widget::measure` call
    // `should_show_label_chip` itself makes on these two widgets, read
    // directly here so the crossover width below is derived from this
    // environment's actual font metrics rather than a literal.
    let (measure_window, measure_label, measure_key) = realized_hint_row(
        activate_key_display.as_deref(),
        &gap_margin_item,
        600,
        window_h,
    );
    assert!(
        measure_label.is_visible(),
        "test bug: a 600px window must be wide enough to show both chips for this item, or the \
         measurements below prove nothing about the real threshold"
    );
    let (_, label_natural, _, _) = measure_label.measure(gtk::Orientation::Horizontal, -1);
    let (_, key_natural, _, _) = measure_key.measure(gtk::Orientation::Horizontal, -1);

    // The real, mapped surface a `window_width` request produces is not
    // `window_width` itself — confirmed directly while writing this test,
    // against this same real broadway display: a `gtk::ScrolledWindow`
    // reserves a fixed amount of extra horizontal space beyond whatever
    // width its own `set_size_request` and the toplevel's
    // `set_default_size` are given (28px in this environment, presumably
    // scrollbar/frame chrome — the exact source does not matter to what
    // this test needs from it, only that it is a fixed offset, not a
    // percentage or a function of the row's own content). `width_offset`
    // measures that gap once here, from the one request
    // (`realized_hint_row`'s `600`) this section already makes for an
    // unrelated reason, and every width this section requests below is
    // pre-compensated by it, so the *actual* surface width `should_show_label_chip`
    // reads back matches the pixel value this test's own arithmetic
    // reasons about, not the raw request that produced it.
    let debug_actual_600 = measure_label
        .root()
        .and_then(|r| r.native())
        .and_then(|n| n.surface())
        .map(|s| s.width())
        .expect("the calibration window must report a real mapped surface width");
    let width_offset = debug_actual_600 - 600;
    measure_window.close();

    let old_needed = *tokens::ICON_SIZE_PX + label_natural + key_natural;
    let new_needed = old_needed + *tokens::HINT_CHIP_GAP_PX + *tokens::HINT_MARGIN_START_PX;
    assert!(
        old_needed > 220,
        "test bug: {gap_margin_item:?}'s label must be long enough that old_needed \
         ({old_needed}px) clears this environment's real floor width, or the crossover width \
         below would be silently clamped up to that floor instead of testing the threshold at \
         all"
    );

    // The crossover width: exactly the *old*, undercounting threshold. The
    // pre-fix sum's own `surface_width >= needed` check is trivially
    // satisfied here (`old_needed >= old_needed`), so a pre-fix
    // implementation shows the label chip at this width even though the
    // hint's real footprint (`new_needed`, `HINT_CHIP_GAP_PX` +
    // `HINT_MARGIN_START_PX` wider) does not actually fit — exactly the
    // "roughly 20px past the width at which it actually overflows" defect
    // this section exists to catch. The fixed implementation must collapse
    // here instead.
    let crossover_width = old_needed;

    let container = row::build();
    let hint_label = row::hint_label_widget(&container)
        .expect("build must give this row a named hint label chip");
    let hint_key =
        row::hint_key_widget(&container).expect("build must give this row a named hint key chip");

    // Step 1: wide — the label shows, never having collapsed at all.
    let (window1, scrolled1) = realize_container_at_width(
        &container,
        activate_key_display.as_deref(),
        &gap_margin_item,
        600,
        window_h,
    );
    assert!(
        hint_label.is_visible(),
        "test bug: a 600px window must show the label chip for this item"
    );
    window1.close();
    scrolled1.set_child(None::<&gtk::Widget>);

    // Step 2: narrow enough to force a real collapse.
    let (window2, scrolled2) = realize_container_at_width(
        &container,
        activate_key_display.as_deref(),
        &gap_margin_item,
        narrow_width,
        window_h,
    );
    assert!(
        !hint_label.is_visible(),
        "test bug: the row must actually collapse at the environment's floor width, or the \
         re-widen assertions below prove nothing about recovering from a real collapse"
    );
    window2.close();
    scrolled2.set_child(None::<&gtk::Widget>);

    // Step 3: the crossover width — proves both halves of this section at
    // once. The threshold fix (the primary defect): this width satisfies
    // the old sum but not the corrected one, so the label chip must stay
    // collapsed. Statelessness (the hazard the review flagged): the row
    // arrives here having *just* collapsed at `narrow_width`, exactly the
    // history a container-measurement implementation would answer
    // differently for than a fresh bind at this same width — and the
    // decision here must match what a fresh bind at `crossover_width` would
    // give regardless.
    let (window3, scrolled3) = realize_container_at_width(
        &container,
        activate_key_display.as_deref(),
        &gap_margin_item,
        crossover_width - width_offset,
        window_h,
    );
    let actual_crossover_surface_width = container
        .native()
        .and_then(|n| n.surface())
        .map(|s| s.width());
    assert_eq!(
        actual_crossover_surface_width,
        Some(crossover_width),
        "test bug: width_offset ({width_offset}px) must land the real surface exactly on \
         crossover_width, or the assertion below is not actually testing the crossover point it \
         claims to"
    );
    assert!(
        !hint_label.is_visible(),
        "at exactly the old, undercounting threshold ({crossover_width}px), the label chip must \
         already have collapsed — its real footprint needs {new_needed}px once the inter-chip \
         gap and the hint's own margin-start are counted, not just {old_needed}px — and this \
         must hold even though the row was just shown wide, then collapsed narrow, immediately \
         before this bind: the same width must give the same answer regardless of what the hint \
         currently shows"
    );
    assert!(
        hint_key.is_visible(),
        "the key glyph must remain even once the label chip collapses at the crossover width"
    );
    window3.close();
    scrolled3.set_child(None::<&gtk::Widget>);

    // Step 4: a genuine re-widen, all the way back out — the chip must
    // still be able to come back once there is genuinely enough room again,
    // ruling out an implementation that can never re-expand once collapsed
    // (the container-measurement hazard `should_show_label_chip`'s own doc
    // comment records rejecting).
    let (window4, _scrolled4) = realize_container_at_width(
        &container,
        activate_key_display.as_deref(),
        &gap_margin_item,
        600,
        window_h,
    );
    assert!(
        hint_label.is_visible(),
        "the label chip must be able to re-expand once genuinely widened, not stay collapsed \
         forever once it has collapsed once"
    );
    window4.close();

    println!("row action hint collapse-threshold and statelessness assertions passed");
}

/// Parents `container` (already built via [`row::build`]) under a fresh,
/// real, presented `gtk::Window` sized to `window_width` × `window_height`,
/// waits for that window to report a genuine mapped surface width under
/// broadway, then binds it to `item` — the bind
/// [`row::should_show_label_chip`] actually measures against, since a bind
/// made before the window has real geometry sees no surface at all (see
/// that function's own "no surface → never collapse" doc section). Returns
/// the window and the `gtk::ScrolledWindow` wrapping `container`, so a
/// caller reusing the same `container` across more than one call can
/// `close()` the window and then `scrolled.set_child(None::<&gtk::Widget>)`
/// to unparent `container` before handing it to a second call — a widget
/// cannot have two parents at once, and `close()` alone only hides a
/// window, it does not clear its child.
///
/// Issue #197 review, finding 1: split out of what used to be
/// `realized_hint_row`'s own body so the collapse-threshold section below
/// can realize the *same* `container` — and therefore the same
/// `hint_label`/`hint_key` widget instances, with whatever visibility state
/// a previous bind left them in — at a sequence of different widths, which
/// is exactly what proving [`row::should_show_label_chip`]'s statelessness
/// needs and what `realized_hint_row`'s original one-shot shape could not
/// provide.
fn realize_container_at_width(
    container: &gtk::Box,
    activate_key_display: Option<&str>,
    item: &Item,
    window_width: i32,
    window_height: i32,
) -> (gtk::Window, gtk::ScrolledWindow) {
    // Wrapped in a `gtk::ScrolledWindow` — matching production
    // (`ui::window::HopWindow::build` wraps its real `GtkListView` in one
    // too) — rather than making `container` the window's direct child.
    // `GtkScrolledWindow::propagate-natural-width` defaults to `false` in
    // GTK4: unlike a plain `gtk::Window`, whose minimum size is always at
    // least its child's own minimum requested size (confirmed directly
    // while writing this test — a bare `container` as the window's direct
    // child left the window unable to shrink below roughly 215px even when
    // asked for 80, because nothing bounded the title label's own minimum
    // width), a scrolled window can be sized *smaller* than its child's
    // natural width, clipping rather than forcing the toplevel to grow.
    // That is exactly the real geometry constraint this test needs to
    // reproduce: a window narrower than its content demands, which is the
    // one situation `should_show_label_chip` exists to react to.
    let scrolled = gtk::ScrolledWindow::builder().child(container).build();
    // `min-content-width` defaults to `-1` ("unset"), which — confirmed
    // directly while writing this test, against a real broadway display —
    // still lets the row's own minimum content width (the title label's
    // ellipsized minimum, plus the icon and hint) propagate up into the
    // window's own minimum size even with `propagate-natural-width` at its
    // GTK4 default of `false`. Pinning it to `1` here is what actually
    // decouples the scrolled window's reported minimum from its child's,
    // so `scrolled.set_size_request` below is not silently overridden by
    // whatever `container` happens to demand.
    scrolled.set_min_content_width(1);
    scrolled.set_size_request(window_width, window_height);

    let window = gtk::Window::new();
    window.set_default_size(window_width, window_height);
    window.set_child(Some(&scrolled));
    window.present();

    assert!(
        wait_until(
            || {
                container
                    .native()
                    .and_then(|native| native.surface())
                    .is_some_and(|surface| surface.width() > 0)
            },
            Duration::from_secs(5),
        ),
        "the test window never reported a real, mapped surface width under broadway"
    );

    // Bind (or rebind) now that the window has a real surface — this is the
    // bind `should_show_label_chip` actually measures against; see that
    // function's own doc comment for why the collapse decision is only
    // ever as fresh as the most recent bind.
    row::bind(
        container.upcast_ref::<gtk::Widget>(),
        item,
        activate_key_display,
    );

    (window, scrolled)
}

/// Builds and binds one `Row` widget inside a real, presented `gtk::Window`
/// sized to `window_width` × `window_height` — a single-shot wrapper around
/// [`realize_container_at_width`] for the (common) case that only needs one
/// width and never reuses the row afterward. Returns the window (so the
/// caller can `close()` it) and the hint's two chip widgets.
fn realized_hint_row(
    activate_key_display: Option<&str>,
    item: &Item,
    window_width: i32,
    window_height: i32,
) -> (gtk::Window, gtk::Label, gtk::Label) {
    let container = row::build();
    let hint_label = row::hint_label_widget(&container)
        .expect("build must give this row a named hint label chip");
    let hint_key =
        row::hint_key_widget(&container).expect("build must give this row a named hint key chip");
    let (window, _scrolled) = realize_container_at_width(
        &container,
        activate_key_display,
        item,
        window_width,
        window_height,
    );
    (window, hint_label, hint_key)
}

/// Pumps the real GLib main context, sleeping briefly between checks, until
/// `condition` returns `true` or `timeout` elapses — returns whether it
/// succeeded.
///
/// A bare, non-blocking `glib::MainContext::iteration(false)` spin was
/// tried first and rejected, for the same reason `app::capture_once_mapped`'s
/// own doc comment gives for rejecting the identical shape there: a
/// headless backend "maps and (re-)size-allocates a surface asynchronously,
/// on the main loop's own schedule," so a non-blocking spin "does nothing
/// when nothing is already pending" — it can return immediately having
/// drained zero sources, long before the broadway socket has actually
/// delivered the configure event this function's callers wait on. This
/// drains whatever is already queued and then yields real wall-clock time
/// (a short `std::thread::sleep`, not a spin) for more of it to arrive over
/// the socket, repeating until `condition` holds or `timeout` is spent —
/// the same "retry across real elapsed time, not just pump the loop"
/// approach `capture_once_mapped` documents choosing, adapted to this
/// file's synchronous `#[test]` body rather than an `async` one.
fn wait_until(mut condition: impl FnMut() -> bool, timeout: Duration) -> bool {
    let ctx = glib::MainContext::default();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        while ctx.iteration(false) {}
        if condition() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The row's reserved layout: the container's own measured height, and the
/// icon's own measured width and height — all three read through
/// `gtk::Widget::measure`'s `natural` component. See the comment at this
/// function's one call site (inside [`run_assertions`]) for why `measure`
/// rather than the `height_request`/`width_request` property getters.
fn row_layout(container: &gtk::Box, icon: &gtk::Image) -> (i32, i32, i32) {
    let (_, container_height, _, _) = container.measure(gtk::Orientation::Vertical, -1);
    let (_, icon_width, _, _) = icon.measure(gtk::Orientation::Horizontal, -1);
    let (_, icon_height, _, _) = icon.measure(gtk::Orientation::Vertical, -1);
    (container_height, icon_width, icon_height)
}

/// A valid, decodable, solid-colour 256x256 grayscale PNG — replacing the
/// 1x1 pixel this test used before review flagged that a 1x1 image cannot
/// tell a correctly-reserved icon slot apart from a regression that
/// dropped its clamp: a 1x1 decoded texture never asks a `gtk::Image` for
/// more than [`tokens::ICON_SIZE_PX`] (26px) regardless of what is or
/// isn't clamping it, so the `row_layout` assertion at this constant's one
/// call site would have passed identically either way. A realistically
/// large image is the fixture a reviewer expects here regardless, and it
/// is what actually exercises `load_path_texture`'s decode path the way a
/// real icon file would.
///
/// What growing this image does *not* do, verified empirically while
/// fixing this (a throwaway probe run directly against `gtk::Image` on
/// this machine's GTK 4.14.5/broadway; the readout is kept in this crate's
/// task-2 fix report, `.superpowers/sdd/issue-190-row-icon/task-2-report.md`,
/// since a future reader re-deriving this from scratch should not have to):
/// give the `row_layout` assertion below the power to catch a regression
/// that drops `ui::row::build`'s [`gtk::Image::set_pixel_size`] call
/// specifically. `gtk::Image::measure` for `Paintable` storage never
/// consults the paintable's own intrinsic size in this GTK version at
/// all — an image with *no* `size_request` and *no* `pixel_size` showing
/// this exact 256x256 texture still measures a 16x16 natural size, not
/// 256x256, and `icon.set_size_request(ICON_SIZE_PX, ICON_SIZE_PX)` (also
/// in `build`, unconditional, independent of which `IconSpec` arm ran) is
/// on its own already sufficient to pin the measured size regardless of
/// `set_pixel_size` or of this image's size. Dropping `set_pixel_size`
/// alone left every assertion in this file passing; only dropping *both*
/// calls together moved the measurement — and it moved on the very first,
/// icon-less baseline capture above, not specifically on this resolved
/// `Path` case. So no image size can make this one assertion regression-test
/// `set_pixel_size` in isolation; what it does buy is a decode exercised on
/// a realistic size instead of a degenerate one, and one less place where a
/// future `gtk::Image` replaced by something that *does* read intrinsic
/// paintable size (e.g. `gtk::Picture`) would go unnoticed.
///
/// Generated by a throwaway Python script (`zlib.compress` over 256
/// identical grayscale scanlines, wrapped in hand-built IHDR/IDAT/IEND
/// chunks with their own CRC32s), not checked in as a dependency:
/// `hop-gtk` has no image-encoding crate anywhere in its dependency graph,
/// and pulling one in just to synthesize one test fixture would be a
/// heavier addition than the fixture is worth. A solid-colour image
/// compresses to a few hundred bytes even at 256x256 — this literal is
/// 368 bytes despite decoding to an image (256*256 = 65536 pixels)
/// roughly 65,000 times larger than the 1x1 pixel it replaces.
const LARGE_ICON_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 1, 0, 0, 0, 1, 0, 8, 0, 0,
    0, 0, 121, 25, 247, 186, 0, 0, 1, 55, 73, 68, 65, 84, 120, 218, 237, 208, 1, 1, 0, 0, 8, 195,
    160, 71, 95, 116, 131, 8, 17, 88, 207, 77, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8,
    16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8,
    16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8,
    16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8,
    16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8,
    16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8,
    16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8,
    16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8,
    16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8,
    16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8,
    16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8, 16, 32, 64, 128, 0, 1, 2, 4, 8,
    168, 3, 249, 124, 7, 129, 166, 92, 59, 145, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// Builds an [`IconName`] from a plain `&str`, panicking on a value that
/// would break one of that type's own rules — every name this file passes
/// in is a short, ASCII, control-free literal, so a failure here is a bug
/// in the test, not a case worth a `Result`.
fn icon_name(name: &str) -> IconName {
    IconName::new(name).expect("test icon name must pass IconName's own rules")
}

/// Builds an [`IconPath`] from a filesystem `path`, panicking the same way
/// [`icon_name`] does — every path this file passes in comes from a
/// `tempfile` directory, which is always absolute.
fn icon_path(path: &std::path::Path) -> IconPath {
    IconPath::new(
        path.to_str()
            .expect("tempfile paths on this platform are valid UTF-8")
            .to_string(),
    )
    .expect("test icon path must pass IconPath's own rules")
}

/// A tiny [`Item`] carrying `spec` as its icon.
fn item_with_icon(n: usize, title: &str, spec: IconSpec) -> Item {
    let mut item = test_item(n, title);
    item.icon = Some(spec);
    item
}

/// A tiny [`Item`] carrying `subtitle` as its `Some(ItemSubtitle)`, `icon`
/// left `None` — the icon section above already exercises every `IconSpec`
/// arm, so the subtitle section has no reason to combine the two.
fn item_with_subtitle(n: usize, title: &str, subtitle: &str) -> Item {
    let mut item = test_item(n, title);
    item.subtitle = Some(
        ItemSubtitle::new(subtitle).expect("test subtitle must pass ItemSubtitle's own rules"),
    );
    item
}

/// A tiny [`Item`] with `icon` left `None` — spelled out as its own
/// function (rather than a bare `test_item(n, title)` call) only so every
/// binding site in the icon section above reads the same way: one call,
/// one icon state, named.
fn item_with_icon_none(n: usize, title: &str) -> Item {
    test_item(n, title)
}

/// A tiny [`Item`] whose `default_action` names an id absent from `actions`
/// entirely — the malformed case `ui::row::default_action_label`'s own doc
/// comment names, and the item-side half of this issue's "both halves or
/// neither" hint rule (see `ui::row::resolve_hint`'s own doc comment).
/// `test_item`'s own default action ("open") is deliberately not reused
/// here as the mismatch, so a future change to `test_item` could not
/// accidentally make this item's `default_action` start matching again
/// without this function's own literal changing too.
fn item_with_actions(n: usize, title: &str) -> Item {
    let mut item = test_item(n, title);
    item.default_action = ActionId::new("archive").unwrap();
    item
}

/// A tiny [`Item`] with zero actions — a legitimate wire shape
/// (`Item.actions` is not required to be non-empty) issue #254's own
/// section uses to prove every action-icon button hides, not just fails to
/// show a first one.
fn item_with_no_actions(n: usize, title: &str) -> Item {
    let mut item = test_item(n, title);
    item.actions = vec![];
    item
}

/// A tiny [`Item`] carrying exactly `ui::row::ROW_ACTION_ICON_CAP` (2)
/// actions of two different [`ActionKind`]s, so issue #254's own section
/// can tell the row's two action-icon buttons apart by more than position.
fn item_with_two_actions(n: usize, title: &str) -> Item {
    let mut item = test_item(n, title);
    item.actions = vec![
        Action {
            id: ActionId::new("open").unwrap(),
            kind: ActionKind::Open,
            label: "Open".to_string(),
        },
        Action {
            id: ActionId::new("copy-path").unwrap(),
            kind: ActionKind::Copy,
            label: "Copy path".to_string(),
        },
    ];
    item.default_action = ActionId::new("open").unwrap();
    item
}

/// [`item_with_two_actions`] plus one more action — one over
/// `ui::row::ROW_ACTION_ICON_CAP` — issue #254's own proof that the row's
/// fixed two icon buttons show exactly the first two actions in wire
/// order, truncating rather than shifting which two are shown.
fn item_with_three_actions(n: usize, title: &str) -> Item {
    let mut item = item_with_two_actions(n, title);
    item.actions.push(Action {
        id: ActionId::new("reveal").unwrap(),
        kind: ActionKind::Focus,
        label: "Reveal".to_string(),
    });
    item
}

/// A tiny [`Item`] whose *only* action is its own default action, carrying
/// `label` verbatim — the responsive-collapse section's own fixture, which
/// needs a default-action label long enough to actually exceed a real
/// window's available width, unlike every other item this file builds
/// (`test_item`'s own "Open" is deliberately short, so the hint content
/// itself was never the thing making the row's other layout assertions
/// hold).
fn item_with_default_action_label(n: usize, title: &str, label: &str) -> Item {
    let mut item = test_item(n, title);
    item.actions = vec![Action {
        id: ActionId::new("open").unwrap(),
        kind: ActionKind::Open,
        label: label.to_string(),
    }];
    item.default_action = ActionId::new("open").unwrap();
    item
}

/// The exact item [`run_assertions`]'s `unbind` call at the end of the icon
/// section stands in for — the same shape `item_b.clone()` plays for the
/// title's own `unbind` assertion earlier in that function: a real
/// `connect_unbind` handler would read `list_item.item()` and get back
/// whatever was most recently bound, and this file has no live
/// `gtk::ListItem` bound to a real list to read that back out of, so it
/// rebuilds the same item by hand instead.
fn folder_item_for_unbind() -> Item {
    item_with_icon(
        5,
        "has a real icon name",
        IconSpec::Name(icon_name("folder")),
    )
}

/// A tiny [`Item`]; `n` differentiates ids so a future assertion could tell
/// two instances apart, matching the shape `crates/hopd/tests/lifecycle.rs`'s
/// own `item` helper uses for the same reason.
fn test_item(n: usize, title: &str) -> Item {
    Item {
        id: ItemId::new(format!("test:{n}")).unwrap(),
        kind: Kind::Action,
        title: ItemTitle::new(title).unwrap(),
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
