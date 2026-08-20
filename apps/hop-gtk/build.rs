//! Compiles `assets/hop-gtk.gresource.xml` (issue #198) into a single
//! `.gresource` blob under `OUT_DIR`, which `src/fonts.rs` embeds with
//! [`gio::resources_register_include!`] — see that module's doc comment for
//! the runtime half of this story. This is the first `build.rs`
//! `apps/hop-gtk` has ever had; every other GTK/libadwaita binding it
//! depends on links against a system `.so` found by a `-sys` crate's own
//! `pkg-config` probe (`apps/hop-gtk/Cargo.toml`'s `gtk4-layer-shell`
//! comment), never a build script of this crate's own.
//!
//! # Why `glib-build-tools`, and what it does and does not handle
//!
//! [`glib_build_tools::compile_resources`] is the `gtk-rs` project's own
//! thin wrapper around shelling out to `glib-compile-resources` — MIT
//! licensed, on `deny.toml`'s allow list already (the workspace-wide MIT
//! entry, not a per-crate exception), so pulling it in as a `[build-
//! dependencies]`-only crate adds nothing to `cargo deny check licenses`
//! that was not already accepted. It does two things this build needs and
//! would otherwise have to hand-roll: it runs `glib-compile-resources
//! --target <target> <gresource>` to produce the compiled blob, and it runs
//! `glib-compile-resources --generate-dependencies <gresource>` and turns
//! every line of that second command's output into its own
//! `cargo:rerun-if-changed=<path>` line — *in addition* to one for the
//! `<gresource>` XML file itself. `--generate-dependencies` walks
//! `assets/hop-gtk.gresource.xml`'s own `<file>` entries and resolves each
//! one against `--sourcedir`, so as long as `source_dirs` below is passed
//! as an absolute path (it is — [`fonts_dir`] canonicalizes it), the
//! dependency lines that come back are themselves absolute paths naming
//! every one of the five real font files under `assets/fonts/` — confirmed
//! directly against this exact combination before writing this comment:
//! `glib-compile-resources --sourcedir=<abs assets/fonts>
//! --generate-dependencies assets/hop-gtk.gresource.xml` printed all five
//! fonts' absolute paths, one per line. So this file does not need its own
//! `cargo:rerun-if-changed` loop over `assets/fonts/*.ttf` — asking for it a
//! second time here would just be a fragile, hand-maintained duplicate of
//! what `--generate-dependencies` already derives correctly from the
//! manifest itself, and would silently stop covering a font added to the
//! XML without a matching line added here.
//!
//! `--sourcedir` is `assets/fonts` specifically, not `assets` — the XML's
//! own `<file>` entries are bare filenames (`Inter-Regular.ttf`, not
//! `fonts/Inter-Regular.ttf`), which its own top comment explains was the
//! fix for a real, confirmed bug: `glib-compile-resources` uses a `<file>`
//! element's text as the resource's sub-path under `prefix` verbatim, so an
//! earlier `assets`-sourced, `fonts/`-prefixed draft of the XML produced
//! resources at a doubled `/dev/hop/Launcher/fonts/fonts/...` path that
//! `src/fonts.rs`'s `FACES` table — correctly — never looked up.
//!
//! # The version mismatch this crate's environment has, and why it is not
//! worked around here
//!
//! This machine has two `glib-compile-resources` binaries: Homebrew's, at
//! `/home/linuxbrew/.linuxbrew/bin/glib-compile-resources` (2.88.2), which
//! is what an ordinary `$PATH` lookup resolves to first in this dev
//! environment, and the system one, at `/usr/bin/glib-compile-resources`
//! (2.80.0) — the version that matches the system `libglib-2.0.so.0` this
//! crate's own compiled binary actually links against (confirmed with
//! `ldd` against the built `hop-gtk` executable). [`compile_resources`]
//! resolves the binary by bare name (`Command::new("glib-compile-
//! resources")`), which cannot be steered from here without either mutating
//! this process's `$PATH` — `std::env::set_var` is an `unsafe fn` as of the
//! 2024 edition this workspace builds under (root `Cargo.toml`: `edition =
//! "2024"`), and `unsafe_code = "deny"` in that same file rules it out flatly
//! — or abandoning the crate's own function in favor of reimplementing its
//! two `Command` invocations by hand, just to hardcode `/usr/bin/...`.
//!
//! Neither was needed: this was verified directly rather than assumed
//! before choosing to leave it alone. A `.gresource` compiled with each of
//! the two binaries above, loaded through this exact workspace's own `gio`
//! 0.22 (linked against the system `libglib-2.0.so.0` the real `hop-gtk`
//! binary links against, confirmed with `ldd`) via `Resource::from_data`
//! followed by `resources_lookup_data`, both succeeded and returned
//! byte-identical font data — the GResource container format the two
//! `glib-compile-resources` versions emit is unchanged between 2.80 and
//! 2.88 in every way that matters to a reader this old or newer. Nothing
//! here forces one binary over the other, and `fonts.rs`'s own test suite
//! (`resource_paths_resolve_to_nonempty_bytes` and its siblings) re-proves
//! this exact "compiled by whatever `glib-compile-resources` this
//! environment's `$PATH` resolves to, loaded by the system `gio` the real
//! binary links against" combination on every `cargo test -p hop-gtk` run —
//! so a future `glib-compile-resources` release that *did* break this
//! compatibility would fail loudly there, on the machine that built it,
//! rather than only in a shipped binary nobody had reason to suspect.
//! Should that ever happen, the fix is exactly the one this comment's
//! opening paragraph ruled out for today: stop calling
//! [`compile_resources`] and shell out to `/usr/bin/glib-compile-resources`
//! by its absolute path directly, falling back to a bare `$PATH` lookup
//! only where `/usr/bin/glib-compile-resources` does not exist (a
//! non-Linux dev machine, say) — no `unsafe` required for that either, an
//! absolute-path `Command::new` is exactly as safe as a bare-name one.

use std::path::{Path, PathBuf};

/// Resolves `assets/`, two directories up from this crate's own manifest —
/// `apps/hop-gtk/Cargo.toml` — canonicalized to an absolute path.
/// Canonicalizing (rather than handing `glib-compile-resources` the bare
/// relative `../../assets`) is what makes the `cargo:rerun-if-changed`
/// lines [`glib_build_tools::compile_resources`] derives from
/// `--generate-dependencies`'s output themselves absolute — see this file's
/// top doc comment for why that is load-bearing, not cosmetic: a relative
/// path in a `cargo:rerun-if-changed` line is resolved against the build
/// script's own working directory (`apps/hop-gtk`, which Cargo guarantees),
/// so a relative `../../assets/fonts/Inter-Regular.ttf` would technically
/// still work — but `--generate-dependencies` only emits paths in the same
/// shape it was given `--sourcedir` in, and passing an absolute
/// `--sourcedir` is what keeps every step of this pipeline (the `--target`
/// compile, the `--generate-dependencies` walk, and this function's own
/// return value) agreeing on one unambiguous path rather than mixing
/// relative and absolute forms.
fn assets_dir() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let assets = manifest_dir.join("../../assets");
    assets.canonicalize().unwrap_or_else(|err| {
        panic!(
            "hop-gtk/build.rs: could not resolve {} (expected the repo's assets/ directory, two \
             levels up from apps/hop-gtk): {err}",
            assets.display()
        )
    })
}

/// `assets/fonts`, the `--sourcedir` `assets/hop-gtk.gresource.xml`'s bare
/// (`fonts/`-unprefixed) `<file>` entries are resolved against — see this
/// file's top doc comment, and that XML file's own, for why sourcing from
/// `assets` itself instead would double the `fonts/` path segment in every
/// resulting resource path.
fn fonts_dir(assets_dir: &Path) -> PathBuf {
    let fonts = assets_dir.join("fonts");
    fonts.canonicalize().unwrap_or_else(|err| {
        panic!(
            "hop-gtk/build.rs: could not resolve {} (expected the repo's assets/fonts/ \
             directory): {err}",
            fonts.display()
        )
    })
}

fn main() {
    let assets_dir = assets_dir();
    let fonts_dir = fonts_dir(&assets_dir);
    let gresource_xml = assets_dir.join("hop-gtk.gresource.xml");

    // `target` is relative to `OUT_DIR` — `glib_build_tools::compile_resources`'s
    // own doc comment says so, and `src/fonts.rs`'s
    // `gio::resources_register_include!("hop-gtk.gresource")` call is
    // written against that same relative name, via the macro's own
    // `concat!(env!("OUT_DIR"), "/", $path)` expansion.
    glib_build_tools::compile_resources(
        &[&fonts_dir],
        gresource_xml.to_str().unwrap_or_else(|| {
            panic!(
                "hop-gtk/build.rs: {} is not valid UTF-8",
                gresource_xml.display()
            )
        }),
        "hop-gtk.gresource",
    );
}
