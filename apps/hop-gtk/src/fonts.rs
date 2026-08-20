//! Bundles hop's two typefaces (issue #198) — the runtime half of the story
//! `apps/hop-gtk/build.rs` starts at compile time by compiling
//! `assets/hop-gtk.gresource.xml` into a `.gresource` blob under `OUT_DIR`.
//! `assets/fonts/README.md` has the full provenance, licensing, and "why
//! exactly these five files" account; `assets/tokens.css`'s own header
//! states the requirement this module exists to satisfy: "Both are bundled
//! via GResource rather than trusted to be installed. A launcher cannot let
//! its identity element fall back silently to generic `monospace` on a
//! fresh install."
//!
//! # What this module does, in order
//!
//! [`bundle`] does three things, once, the first time anything calls it (see
//! "Computed once" below):
//!
//! 1. **Registers** the compiled resource with gio, via
//!    [`gio::resources_register_include!`] — see "Why the macro, not
//!    `gio::Resource::from_data` directly" below.
//! 2. **Materializes** every [`FACES`] entry's bytes to a real file on disk,
//!    in a fresh directory this module holds for the process's life — see
//!    "Why disk, not memory" below for why this step exists at all, and
//!    "The directory: `$XDG_RUNTIME_DIR`, not `/tmp`" for why that
//!    particular parent.
//! 3. **Registers that directory with fontconfig**, via a single
//!    `FcConfigAppFontAddDir` call, so the materialized faces actually
//!    resolve through Pango's font map — a registered GResource and a
//!    materialized file on disk are both necessary but neither is
//!    sufficient; fontconfig has to be told the directory exists before
//!    anything asks Pango to lay out `"Inter"` or `"Iosevka Term"`. See
//!    "Registering with fontconfig" below for the mechanism, the one
//!    `unsafe` block this needs, and why a directory-wide call rather than
//!    five per-file ones.
//!
//! An earlier revision of this doc comment described step 3 as "the
//! issue's other, still-open half" and named this module as deliberately
//! stopping short of it. That is no longer true — see "Registering with
//! fontconfig" for what changed and under what maintainer-approved terms.
//!
//! # Why the macro, not `gio::Resource::from_data` directly
//!
//! [`gio::Resource::from_data`] takes a [`glib::Bytes`] and hands it to
//! `g_resource_new_from_data`, which historically required the byte buffer
//! to start at an 8-byte-aligned address (GNOME bug 790030) — a real hazard
//! for `include_bytes!`, which makes no alignment guarantee about where the
//! embedded array ends up in the binary's `.rodata`. `gio` 0.22's own
//! `Resource::from_data` (confirmed by reading
//! `gio-0.22.8/src/resource.rs`) already works around this itself: it reads
//! the pointer back out of the `glib::Bytes` it was given and, if it is not
//! aligned, clones into a freshly, correctly-aligned allocation before
//! calling into `g_resource_new_from_data` — so the historical hazard is not
//! actually reachable through the safe API either way on this `gio` version.
//! [`gio::resources_register_include!`] is still preferred here over calling
//! `from_data` and `resources_register` by hand, because it is the
//! `gtk-rs`-documented pairing for exactly this build-script-produced
//! artifact (its own doc comment: "Include gresources generated with
//! `glib_build_tools::compile_resources`") and because it is the one
//! `include_bytes!` call site this crate needs — writing the equivalent by
//! hand would just be this macro's own two-line expansion, copied instead of
//! reused, for a component this module's own `Cargo.toml` comment already
//! names as the crate's first build script.
//!
//! # Why disk, not memory
//!
//! Fontconfig 2.15.0 — the version this issue's environment has (confirmed
//! with `fc-cache -V`) — offers `FcConfigAppFontAddFile` and
//! `FcConfigAppFontAddDir`, both of which take a *path*, and has no
//! `FcConfigAppFontAddMemFile` or equivalent: there is no fontconfig entry
//! point this crate could call with the bytes [`gio::resources_lookup_data`]
//! already hands back that would register a face without a file on disk
//! somewhere first. So even though the eventual fontconfig-registration step
//! is explicitly out of this issue's scope (see "What this issue does not
//! close"), the materialization step it will need is not: whatever calls
//! `FcConfigAppFontAddFile` next needs a real path to give it, and this
//! module is what produces one.
//!
//! The files must also *stay* on disk for the rest of the process's life,
//! not just long enough to hand fontconfig a path once: fontconfig's own
//! registration records only the path, and FreeType — the library that
//! actually reads a glyph's outline data — opens that path lazily, the
//! first time something actually needs to *rasterize* a glyph from that
//! face, not at registration time. A file deleted between registration and
//! the first paint would make that first rasterization fail, invisibly,
//! for a face fontconfig still believes is available. This is why
//! [`bundle`]'s [`FontBundle`] is held in a process-lifetime [`LazyLock`]
//! (see "Computed once" below) rather than materialized into a
//! function-local [`tempfile::TempDir`] that would delete its contents the
//! moment that function returned.
//!
//! # The directory: `$XDG_RUNTIME_DIR`, not `/tmp`
//!
//! [`tempfile::Builder::tempdir_in`] is rooted at `$XDG_RUNTIME_DIR`
//! specifically, not `/tmp` or `$XDG_CACHE_HOME`, both of which
//! [`std::env::temp_dir`] or a bare [`tempfile::tempdir`] would have reached
//! for instead. `$XDG_RUNTIME_DIR` is a `tmpfs` the desktop session manager
//! creates and tears down with the session (the freedesktop base directory
//! specification: "This directory is removed when the user logs out"), which
//! is what bounds the leak a `kill -9`'d `hop-gtk` — one that never runs its
//! own `Drop` and therefore never deletes its own materialized directory —
//! can cause: at worst, a few megabytes of font files that vanish at the
//! next logout regardless. `/tmp` has no such guarantee (a systemd
//! `tmp-clean` timer or a reboot might clear it, or might not, depending on
//! distro policy); `$XDG_CACHE_HOME` is explicitly meant to persist across
//! reboots, which is the opposite of what a `kill -9`-orphaned scratch
//! directory should do.
//!
//! `--screenshot` is [`gio::ApplicationFlags::NON_UNIQUE`] (`app.rs`'s
//! `run_screenshot` doc comment), so several `hop-gtk --screenshot`
//! processes can and do run concurrently against the same
//! `$XDG_RUNTIME_DIR` — a CI job fanning captures out in parallel, say.
//! [`tempfile::Builder::tempdir_in`]'s random suffix (the same collision-
//! avoidance mechanism `icon_roots.rs`'s own tests already lean on for
//! their scratch directories) is what makes that safe: every concurrent
//! process gets its own uniquely-named directory under the same parent,
//! never contending over one shared path.
//!
//! # Computed once, not per lookup
//!
//! [`BUNDLE`] follows [`crate::icon_roots::ALLOWED_ICON_ROOTS`]'s own shape
//! — a [`std::sync::LazyLock`] computed once, on first access, and reused
//! for the rest of the process's life — for the identical reason: gio
//! resource registration is itself a global, process-wide, idempotent-once
//! side effect (`g_resources_register` adds to a process-wide search list;
//! nothing here should call it twice), and the materialized directory must
//! outlive every caller that reads a path out of it, which only a
//! process-lifetime owner can guarantee.
//!
//! Unlike [`crate::icon_roots::ALLOWED_ICON_ROOTS`], whose construction
//! cannot fail (every environment variable it reads is optional, with a
//! documented fallback for `None`), registering a resource, materializing a
//! face, or registering the materialized directory with fontconfig
//! genuinely can: a corrupt `.gresource` blob, a full or unwritable
//! `$XDG_RUNTIME_DIR`, a missing `$XDG_RUNTIME_DIR` entirely, or fontconfig
//! itself refusing the directory. [`BUNDLE`]
//! therefore holds a `Result`, not a bare [`FontBundle`] — [`bundle`] hands
//! back a borrow of whichever variant [`init`] actually produced, once,
//! rather than baking a `panic!` into the `LazyLock`'s own initializer
//! (which nothing downstream could recover from as a typed error) or
//! `unwrap`/`expect`-ing a value this module cannot promise is `Ok` (a
//! `clippy::unwrap_used` warning this workspace already treats as
//! `-D warnings` under `cargo clippy -p hop-gtk --all-targets`). This is
//! the fail-loudly requirement issue #198 itself exists to enforce, stated
//! as a type rather than a comment: **a caller that ignores [`bundle`]'s
//! `Err` and reaches for a system font instead has to write that fallback
//! itself, in full view of a reviewer** — there is no code path in this
//! module that quietly does it for them.
//!
//! # Registering with fontconfig
//!
//! Every fontconfig entry point capable of registering an application font
//! takes a raw path or directory through FFI — `FcConfigAppFontAddFile`,
//! `FcConfigAppFontAddDir` — so calling one needs an `unsafe` block, which
//! `unsafe_code = "deny"` in the workspace's `[workspace.lints.rust]` (root
//! `Cargo.toml`) does not allow silently. An earlier revision of this
//! module stopped short of this step for exactly that reason, deferring the
//! decision to whoever owned the call. That deferral was raised with the
//! maintainer mid-implementation and **approved** — issue #198's own
//! comment thread records the waiver in full ("Acceptance criterion
//! waived: 'No new `unsafe`'"), including why the two candidate *safe*
//! paths do not exist on this machine: `pango::FontMap::add_font_file` is a
//! safe binding, but it is gated behind the `pango` crate's `v1_56`
//! feature, which needs pango ≥ 1.55 and this machine has 1.52.1 (`nm -D
//! libpango-1.0.so.0` confirms the symbol itself is absent, not merely
//! unbound); the `fontconfig` crate (0.11.0), despite billing itself as "a
//! safe, higher-level wrapper", exposes only query and matching, with no
//! `AppFontAdd*` surface at all, safe or otherwise. What was approved
//! instead is exactly what [`register_with_fontconfig`] does: one narrow
//! `#[expect(unsafe_code)]` around a single `FcConfigAppFontAddDir` call,
//! plus the new `yeslogic-fontconfig-sys` dependency (MIT, already on
//! `deny.toml`'s allow list, links the system fontconfig via `pkg-config`
//! and vendors no C source of its own) that call needs.
//!
//! ## One `AddDir` call, not five `AddFile` calls
//!
//! `FcConfigAppFontAddDir` scans every font file in a directory and adds
//! each one it recognizes to the current config's application-font set;
//! `FcConfigAppFontAddFile` does the identical thing for one named file.
//! The two are genuinely equivalent here, not merely similar enough: the
//! directory [`init`] passes to [`register_with_fontconfig`] is the same
//! [`tempfile::TempDir`] this module's own materialization step just
//! finished writing exactly [`FACES`]'s five files into, and nothing
//! else — see "The directory: `$XDG_RUNTIME_DIR`, not `/tmp`" above for why
//! it is always freshly created and never shared or reused across
//! processes. A directory scan over a directory whose contents this module
//! itself just wrote finds exactly the same five files five `AddFile` calls
//! would have named individually, at the cost of one FFI call (and one
//! `unsafe` block, one `SAFETY` comment, one failure mode to handle) instead
//! of five. Were this directory ever shared with unrelated files — a real
//! risk for, say, `$XDG_RUNTIME_DIR` itself, or a directory another process
//! also writes into — `AddDir`'s "scan everything" behavior would stop
//! being equivalent to naming five specific files, and this choice would
//! need revisiting.
//!
//! ## What `NULL` as the `FcConfig*` argument means
//!
//! `FcConfigAppFontAddDir`'s first argument is a `*mut FcConfig`.
//! freedesktop's own fontconfig reference documents passing `NULL` there as
//! shorthand for "the current configuration" — the same config Pango's font
//! map consults when it resolves a family name — which is what
//! [`register_with_fontconfig`] passes: this module has no config object of
//! its own to hand back, and does not want one; it wants whatever config
//! Pango is about to use.
//!
//! ## The ordering hazard — read this before moving this call
//!
//! `FcConfigAppFontAddDir` must run **before** Pango constructs its first
//! font map. If it runs after, the bundled faces are invisible to that font
//! map and there is no recovery: the reload entry point,
//! `pango_fc_font_map_config_changed`, is not exposed anywhere in the Rust
//! `pango` bindings (confirmed while investigating this waiver), so a
//! caller on the wrong side of that ordering cannot fix it by calling
//! something else afterward — the only fix is to not get the ordering
//! wrong. This is why [`bundle`] (and therefore this registration) is
//! forced from `app::run`, before `adw::Application::new` is constructed in
//! either run mode, rather than from `connect_startup` alongside
//! `style::install` — see `app.rs`'s own doc comment on that call site for
//! the fuller argument. [`register_with_fontconfig`] itself does not, and
//! cannot, enforce this ordering; it is a property of *where this module is
//! called from*, not of anything inside it.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use thiserror::Error;

/// One bundled font face: which family it belongs to, which weight it is,
/// and where its bytes live in the compiled GResource — the "one place"
/// issue #198 asks this data to live, cross-referenced against
/// `assets/tokens.css` lines 40–49 by [`crate::tokens::text_token_names`]
/// and [`crate::tokens::font_token`] in this module's own tests (see
/// `bundled_faces_cover_every_weight_tokens_css_declares`, below) rather
/// than merely by a comment asserting the two agree.
///
/// Mirrors `assets/fonts/README.md`'s own table exactly — five rows, same
/// files, same families, same weights — because that table *is* the
/// authoring record this struct restates as data a test can check instead
/// of only a human reading prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Face {
    /// The font family name as embedded in the file itself and as
    /// `--hop-font-sans`/`--hop-font-mono` name it — `"Inter"` or
    /// `"Iosevka Term"`, confirmed against each file with `fc-query
    /// -f '%{family}|%{style}|%{weight}\n'` (`assets/fonts/README.md`'s own
    /// "Which faces, and why exactly these" section).
    pub family: &'static str,
    /// The OpenType/CSS numeric weight this file provides — `400`, `500`,
    /// or `600`. No two [`FACES`] entries share both this and [`family`].
    pub weight: u16,
    /// This face's path inside the compiled GResource, under the
    /// `/dev/hop/Launcher/fonts/` prefix `assets/hop-gtk.gresource.xml`
    /// declares (matching `app.rs`'s own `APP_ID`, `"dev.hop.Launcher"`,
    /// via GNOME's usual reverse-DNS-to-path convention for a resource
    /// namespace). [`gio::resources_lookup_data`] resolves this exact
    /// string once [`bundle`] has registered the resource.
    pub resource_path: &'static str,
}

impl Face {
    /// The plain filename this face materializes to on disk — the last
    /// path segment of [`Self::resource_path`], which is also, by
    /// construction, `assets/fonts/`'s own filename for this face (the XML
    /// declares each `<file>` by that same bare name, joined onto the
    /// gresource's `prefix` attribute to form `resource_path`). Computed
    /// from `resource_path` rather than stored as a sixth field, so there
    /// is exactly one string per face this module could get out of sync
    /// with itself, not two.
    ///
    /// The `unwrap_or` fallback is unreachable in practice — every
    /// `resource_path` in [`FACES`] contains at least one `/`, so
    /// `rsplit('/')` always yields at least one non-empty final segment —
    /// but it costs nothing to make total rather than reach for `.expect()`
    /// on a value this function's own caller (every one of them, today)
    /// does not construct, and could in principle be a `Face` value that
    /// did not come from [`FACES`] at all.
    fn file_name(&self) -> &'static str {
        self.resource_path
            .rsplit('/')
            .next()
            .unwrap_or(self.resource_path)
    }
}

/// The five bundled faces — `assets/fonts/README.md`'s own table, restated
/// as data. See [`Face`]'s own doc comment for what each field means and
/// how it is cross-checked against `assets/tokens.css`.
///
/// Every `resource_path` below repeats the same `/dev/hop/Launcher/fonts/`
/// prefix `assets/hop-gtk.gresource.xml` declares on its `<gresource>`
/// element (matching `app.rs`'s own `APP_ID`, `"dev.hop.Launcher"`) —
/// spelled out in full five times rather than built from one shared `const`
/// prefix, because `concat!` needs its arguments to be literals and a
/// `const` cannot be spliced into one at compile time. The XML's own
/// declaration and these five literals are therefore two independent
/// places that must agree, not one enforced by the compiler —
/// `resource_paths_resolve_to_nonempty_bytes`, this module's own test
/// below, is the runtime check that catches a drift between them: a
/// `resource_path` this list names that the compiled resource does not
/// actually contain fails that test's `gio::resources_lookup_data` call
/// immediately.
pub static FACES: &[Face] = &[
    Face {
        family: "Inter",
        weight: 400,
        resource_path: "/dev/hop/Launcher/fonts/Inter-Regular.ttf",
    },
    Face {
        family: "Inter",
        weight: 500,
        resource_path: "/dev/hop/Launcher/fonts/Inter-Medium.ttf",
    },
    Face {
        family: "Inter",
        weight: 600,
        resource_path: "/dev/hop/Launcher/fonts/Inter-SemiBold.ttf",
    },
    Face {
        family: "Iosevka Term",
        weight: 400,
        resource_path: "/dev/hop/Launcher/fonts/IosevkaTerm-Regular.ttf",
    },
    Face {
        family: "Iosevka Term",
        weight: 500,
        resource_path: "/dev/hop/Launcher/fonts/IosevkaTerm-Medium.ttf",
    },
];

/// One [`Face`], paired with the real path its bytes were written to on
/// disk — [`FontBundle::faces`]'s element type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedFace {
    pub face: Face,
    /// Where [`Face::resource_path`]'s bytes now live on disk, inside
    /// [`FontBundle::dir`]. Stable for the process's life — see this
    /// module's doc comment, "Why disk, not memory", for why that
    /// stability is load-bearing and not just a convenience.
    pub path: PathBuf,
}

/// The registered resource's materialized state: every [`FACES`] entry's
/// bytes, written to a real file under [`Self::dir`]. Built exactly once by
/// [`init`], behind [`BUNDLE`] — see this module's doc comment, "Computed
/// once, not per lookup".
pub struct FontBundle {
    /// Held for its `Drop` impl alone: a [`tempfile::TempDir`] deletes its
    /// directory (and everything under it) when dropped, which must not
    /// happen before the process itself exits — see this module's doc
    /// comment, "Why disk, not memory", for why FreeType needs this path to
    /// keep resolving for as long as any glyph from these faces might still
    /// be rasterized. [`BUNDLE`]'s `'static` `LazyLock` is what keeps this
    /// `Drop` from ever running early: nothing owns a `FontBundle` by value
    /// anywhere in this crate.
    dir: tempfile::TempDir,
    /// Parallel in content to [`FACES`] (same length, same order, built by
    /// one pass over it in [`init`]), but kept as its own `Vec` rather than
    /// zipped with `FACES` on every read — a [`MaterializedFace`] carries
    /// its own [`PathBuf`], which [`FACES`]'s `'static` [`Face`] entries
    /// have no field for.
    faces: Vec<MaterializedFace>,
}

impl FontBundle {
    /// The directory every bundled face was materialized into. Exists, and
    /// will keep existing, for as long as this [`FontBundle`] does — which,
    /// reached only through [`bundle`], is the rest of the process's life.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// Every bundled face, paired with the real on-disk path its bytes were
    /// written to — the join between [`FACES`]'s static, compile-time data
    /// and this run's own materialization.
    pub fn faces(&self) -> &[MaterializedFace] {
        &self.faces
    }
}

/// Every way registering the compiled resource or materializing a face to
/// disk can fail — issue #198's own requirement, "fail loudly": a typed
/// refusal, matching [`crate::keymap::KeymapError`]'s shape (one variant per
/// distinct failure, each naming exactly what went wrong and where), rather
/// than a caller falling back to a system font lookup it had to invent for
/// itself because [`bundle`] gave it nothing to refuse with.
#[derive(Debug, Error)]
pub enum FontsError {
    /// [`gio::resources_register_include!`] itself failed — a malformed or
    /// corrupt compiled `.gresource` blob. Should not be reachable in an
    /// ordinary build (`build.rs` and `glib-compile-resources` are what
    /// produce that blob from `assets/hop-gtk.gresource.xml`, and this
    /// module's own tests exercise the result on every `cargo test -p
    /// hop-gtk`), but a `Result` this module hands back rather than
    /// panics over regardless — see this module's doc comment, "Computed
    /// once, not per lookup".
    #[error("failed to register the compiled font resource with gio: {0}")]
    Register(#[source] glib::Error),

    /// Neither this process nor its environment has `$XDG_RUNTIME_DIR` set
    /// — the one parent directory [`init`] will materialize faces under.
    /// See this module's doc comment, "The directory: `$XDG_RUNTIME_DIR`,
    /// not `/tmp`", for why no other location is used instead of refusing.
    #[error(
        "$XDG_RUNTIME_DIR is not set; cannot choose a materialization directory for bundled fonts"
    )]
    MissingRuntimeDir,

    /// [`tempfile::Builder::tempdir_in`] itself failed — `$XDG_RUNTIME_DIR`
    /// named a location that does not exist, is not writable, or is full.
    #[error("failed to create a materialization directory under {}: {source}", parent.display())]
    CreateDir {
        /// The `$XDG_RUNTIME_DIR` value the directory was to be created
        /// under.
        parent: PathBuf,
        /// The underlying IO error.
        #[source]
        source: io::Error,
    },

    /// [`gio::resources_lookup_data`] found no data at a [`Face`]'s own
    /// [`Face::resource_path`] — the resource registered successfully, but
    /// this particular path is not one of the entries it actually contains.
    /// A drift between [`FACES`] and `assets/hop-gtk.gresource.xml`, most
    /// likely.
    #[error("font resource {path} did not resolve to any bytes: {source}")]
    Lookup {
        /// The resource path that failed to resolve.
        path: &'static str,
        /// The underlying glib error.
        #[source]
        source: glib::Error,
    },

    /// The resource's bytes resolved, but writing them to
    /// [`Self::CreateDir`]'s directory failed — a permissions problem, or
    /// the filesystem filled up between creating the directory and writing
    /// into it.
    #[error("failed to materialize {path} to {}: {source}", dest.display())]
    Materialize {
        /// The resource path whose bytes could not be written out.
        path: &'static str,
        /// Where they were being written to.
        dest: PathBuf,
        /// The underlying IO error.
        #[source]
        source: io::Error,
    },

    /// The materialized directory's path could not be turned into a
    /// [`std::ffi::CString`] — the only way it can fail is an interior NUL
    /// byte, which no real Unix path can ever contain (the kernel itself
    /// uses NUL as the path terminator at the syscall boundary, so a path
    /// component containing one could never have been created in the first
    /// place). Unreachable in practice for exactly the reason [`Face::file_name`]'s
    /// own doc comment gives for its `unwrap_or` fallback — but a typed
    /// `Err` costs nothing here either, and this module's whole point is
    /// refusing loudly rather than reaching for `.expect()` on a value nothing
    /// downstream can promise is well-formed.
    #[error(
        "bundled font directory path {} contains an interior NUL byte, which fontconfig's C \
         API cannot represent",
        dir.display()
    )]
    FontconfigPath {
        /// The directory path that could not be converted.
        dir: PathBuf,
    },

    /// `FcConfigAppFontAddDir` returned `FcBool` false — fontconfig itself
    /// refused to add the bundled directory to the current config's
    /// application-font set. fontconfig's own documentation does not
    /// enumerate specific causes (unlike this module's other error
    /// variants, which each wrap a `source` naming exactly what went
    /// wrong), so this variant carries only the directory that was
    /// refused.
    #[error(
        "fontconfig refused to add the bundled font directory {} to the current config",
        dir.display()
    )]
    FontconfigRegister {
        /// The directory fontconfig refused.
        dir: PathBuf,
    },
}

/// The registered, materialized font bundle — see this module's doc
/// comment for the full account of what [`init`] does and why it only
/// needs to run once. `&'static FontsError` on the error side, not an owned
/// or cloned one: the failure — if [`init`] produced one — lives inside
/// [`BUNDLE`] for the process's life exactly like a success would, so every
/// caller observes the identical failure, described identically, rather
/// than each retry attempt re-running (and potentially re-failing
/// differently from) fallible IO and gio calls that already ran once.
pub fn bundle() -> Result<&'static FontBundle, &'static FontsError> {
    BUNDLE.as_ref()
}

/// [`bundle`]'s backing storage — see this module's doc comment, "Computed
/// once, not per lookup", for why this is a `LazyLock<Result<...>>` rather
/// than either a bare `LazyLock<FontBundle>` (whose init closure has no
/// `Result` to return a failure through) or a value [`bundle`] reconstructs
/// on every call.
static BUNDLE: LazyLock<Result<FontBundle, FontsError>> = LazyLock::new(init);

/// Registers the compiled resource and materializes every [`FACES`] entry
/// to disk — [`BUNDLE`]'s own initializer, and the only place either of
/// those two steps happens.
fn init() -> Result<FontBundle, FontsError> {
    gio::resources_register_include!("hop-gtk.gresource").map_err(FontsError::Register)?;

    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or(FontsError::MissingRuntimeDir)?;
    let runtime_dir = PathBuf::from(runtime_dir);

    let dir = tempfile::Builder::new()
        .prefix("hop-gtk-fonts-")
        .tempdir_in(&runtime_dir)
        .map_err(|source| FontsError::CreateDir {
            parent: runtime_dir,
            source,
        })?;

    let mut faces = Vec::with_capacity(FACES.len());
    for face in FACES {
        let path = materialize(face, dir.path())?;
        faces.push(MaterializedFace { face: *face, path });
    }

    // Only after every face is actually on disk: `FcConfigAppFontAddDir`
    // scans `dir.path()`'s contents at the moment it is called, so calling
    // it any earlier — say, interleaved with the materialize loop above —
    // could scan a partially-written directory and silently miss whichever
    // faces had not been written yet.
    register_with_fontconfig(dir.path())?;

    Ok(FontBundle { dir, faces })
}

/// Registers `dir` — [`init`]'s own materialized directory, containing
/// exactly [`FACES`]'s five files and nothing else by the time this is
/// called — with fontconfig's current config, via one
/// `FcConfigAppFontAddDir` call. See this module's doc comment,
/// "Registering with fontconfig", for the maintainer waiver this call site
/// exists under, why one directory-wide call was chosen over five per-file
/// `FcConfigAppFontAddFile` calls, what passing `NULL` as the config
/// argument means, and the ordering hazard that governs where this
/// function's *caller's caller* is allowed to run — this function itself
/// enforces none of that ordering; it only performs the call once asked.
fn register_with_fontconfig(dir: &Path) -> Result<(), FontsError> {
    use std::os::unix::ffi::OsStrExt;

    let c_dir = std::ffi::CString::new(dir.as_os_str().as_bytes()).map_err(|_| {
        FontsError::FontconfigPath {
            dir: dir.to_path_buf(),
        }
    })?;

    // SAFETY:
    // - Pointer validity: `c_dir` is a `CString` that owns its own
    //   NUL-terminated buffer; `c_dir.as_ptr()` is valid for the entire
    //   statement below and is not read after it, so there is no
    //   dangling-pointer or use-after-free hazard.
    // - NUL termination: `CString::new` above is exactly what guarantees
    //   the buffer `c_dir.as_ptr()` points at ends in a NUL byte with no
    //   interior one — the one property `FcConfigAppFontAddDir` needs of a
    //   C string argument.
    // - Thread context: this runs inside [`init`], which runs exactly once,
    //   inside [`BUNDLE`]'s `LazyLock` initializer — `std::sync::LazyLock`
    //   guarantees at most one thread ever executes this closure, and every
    //   other thread that reaches [`bundle`] concurrently blocks until it
    //   finishes. No other call in this process touches fontconfig's
    //   current config, so there is no concurrent-mutation hazard fontconfig
    //   itself would need to guard against.
    // - What `NULL` means: fontconfig's own reference documents `NULL` here
    //   as "the current configuration" — see this module's doc comment,
    //   "What `NULL` as the `FcConfig*` argument means", for why that is
    //   the config this call needs to reach (the same one Pango's font map
    //   consults) rather than one this module would have to create and own.
    #[expect(
        unsafe_code,
        reason = "FcConfigAppFontAddDir is a C API with no safe Rust binding on this pango \
                  version (issue #198's maintainer-approved waiver); the block above is the \
                  narrowest possible — one FFI call, nothing else — per that waiver's own terms"
    )]
    let added = unsafe {
        fontconfig_sys::FcConfigAppFontAddDir(
            std::ptr::null_mut(),
            c_dir.as_ptr().cast::<fontconfig_sys::FcChar8>(),
        )
    };

    if added == 0 {
        return Err(FontsError::FontconfigRegister {
            dir: dir.to_path_buf(),
        });
    }

    Ok(())
}

/// Looks up one [`Face`]'s bytes in the (already registered) resource and
/// writes them to `dir`, under [`Face::file_name`]. The one function
/// [`init`]'s loop calls once per [`FACES`] entry.
fn materialize(face: &Face, dir: &Path) -> Result<PathBuf, FontsError> {
    let data = gio::resources_lookup_data(face.resource_path, gio::ResourceLookupFlags::NONE)
        .map_err(|source| FontsError::Lookup {
            path: face.resource_path,
            source,
        })?;

    let dest = dir.join(face.file_name());
    std::fs::write(&dest, &data).map_err(|source| FontsError::Materialize {
        path: face.resource_path,
        dest: dest.clone(),
        source,
    })?;

    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every [`FACES`] entry's [`Face::resource_path`] must resolve through
    /// the registered resource to real, non-empty bytes — the most basic
    /// claim this module makes: the compiled `.gresource`
    /// `apps/hop-gtk/build.rs` produces actually contains what [`FACES`]
    /// says it does, at the paths it says it does.
    ///
    /// Calls [`bundle`] first, unconditionally, rather than assuming
    /// registration has already happened by the time this test runs:
    /// `cargo test` gives no ordering guarantee between the `#[test]`
    /// functions in one binary, so this may well be the *first* of this
    /// module's tests the test harness happens to run, and
    /// `gio::resources_lookup_data` — a bare, unregistered lookup — fails
    /// exactly the way an unbundled resource path would if nothing had
    /// called [`bundle`] yet.
    #[test]
    fn resource_paths_resolve_to_nonempty_bytes() {
        bundle().unwrap_or_else(|err| panic!("bundle() returned an error: {err}"));

        for face in FACES {
            let data =
                gio::resources_lookup_data(face.resource_path, gio::ResourceLookupFlags::NONE)
                    .unwrap_or_else(|err| panic!("{} did not resolve: {err}", face.resource_path));
            assert!(
                !data.is_empty(),
                "{} resolved to zero bytes",
                face.resource_path
            );
        }
    }

    /// The materialized files this module writes to disk must actually
    /// exist, be non-empty, and byte-match the resource data they were
    /// materialized from — proving the on-disk copy [`FontBundle::faces`]
    /// hands back is not just present, but correct.
    #[test]
    fn materialized_files_exist_and_byte_match_the_resource_data() {
        let bundle = bundle().unwrap_or_else(|err| panic!("bundle() returned an error: {err}"));

        assert_eq!(
            bundle.faces().len(),
            FACES.len(),
            "every FACES entry should have been materialized exactly once"
        );

        for materialized in bundle.faces() {
            let on_disk = std::fs::read(&materialized.path).unwrap_or_else(|err| {
                panic!(
                    "materialized file {} does not exist or could not be read: {err}",
                    materialized.path.display()
                )
            });
            assert!(
                !on_disk.is_empty(),
                "materialized file {} is empty",
                materialized.path.display()
            );

            let resource_data = gio::resources_lookup_data(
                materialized.face.resource_path,
                gio::ResourceLookupFlags::NONE,
            )
            .unwrap_or_else(|err| {
                panic!("{} did not resolve: {err}", materialized.face.resource_path)
            });
            assert_eq!(
                on_disk,
                resource_data.as_ref(),
                "materialized file {} does not byte-match its resource data",
                materialized.path.display()
            );

            assert!(
                materialized.path.starts_with(bundle.dir()),
                "materialized file {} is not inside bundle.dir() ({})",
                materialized.path.display(),
                bundle.dir().display()
            );
        }
    }

    /// [`FACES`] must cover exactly the `(family, weight)` pairs
    /// `assets/tokens.css`'s own `--hop-text-*` declarations ask for — the
    /// high-value regression guard issue #198's brief calls for by name:
    /// this is what stops the bundle silently drifting from the design the
    /// day an eleventh `--hop-text-*` token is added, or an existing one's
    /// weight changes, without a matching `.ttf` landing in
    /// `assets/fonts/`.
    ///
    /// Reuses [`crate::tokens::text_token_names`] and
    /// [`crate::tokens::font_token`] — the same parse `crate::tokens`
    /// already performs for every other structural value this crate reads
    /// out of `assets/tokens.css` — rather than re-scanning the file's text
    /// a second time here. [`crate::tokens::FontToken::family`] is the
    /// fully `var()`-resolved family *list* (e.g. `"Inter", -apple-system,
    /// "Cantarell", sans-serif`, `--hop-font-sans`'s own value) — this test
    /// takes only its first, quoted entry, the same "first entry of the
    /// fallback chain" name [`Face::family`]'s own doc comment already
    /// commits to.
    #[test]
    fn bundled_faces_cover_every_weight_tokens_css_declares() {
        let mut declared: Vec<(String, u16)> = crate::tokens::text_token_names()
            .into_iter()
            .map(|name| {
                let token = crate::tokens::font_token(name);
                let family = first_family(token.family);
                (family, token.weight)
            })
            .collect();
        declared.sort();
        declared.dedup();

        let mut bundled: Vec<(String, u16)> = FACES
            .iter()
            .map(|face| (face.family.to_string(), face.weight))
            .collect();
        bundled.sort();
        bundled.dedup();

        assert_eq!(
            declared, bundled,
            "assets/tokens.css's --hop-text-* tokens declare a (family, weight) set that \
             does not match FACES exactly — either a token asks for a weight no bundled \
             face carries, or FACES carries a face no token actually asks for"
        );
    }

    /// Pulls the first, quoted family name out of a resolved `--hop-font-*`
    /// value — e.g. `"Inter", -apple-system, "Cantarell", sans-serif` →
    /// `Inter` — the same "first entry names the bundled face; the rest are
    /// this crate's own system-font fallback chain, never reached because
    /// this issue's whole point is that the first entry always resolves"
    /// reading `assets/tokens.css`'s own `TYPEFACES` section comment gives
    /// for why a fallback chain sits behind the bundled name at all.
    fn first_family(resolved_family_list: &str) -> String {
        resolved_family_list
            .split(',')
            .next()
            .unwrap_or(resolved_family_list)
            .trim()
            .trim_matches('"')
            .to_string()
    }

    /// [`FontsError`] must actually be the type a failure comes back as —
    /// not a panic, not a silent `Ok` — when a face's resource path does
    /// not resolve. Calls [`materialize`] directly, the real, private
    /// production function [`init`]'s own loop calls once per [`FACES`]
    /// entry, rather than [`init`] itself: `$XDG_RUNTIME_DIR` cannot be
    /// overridden for this one test without mutating process-wide
    /// environment state, which — under this workspace's edition-2024
    /// `std::env::set_var`/`remove_var`, both `unsafe fn` since Rust 1.82 —
    /// this crate's own `unsafe_code = "deny"` lint (root `Cargo.toml`)
    /// rules out. `materialize`'s own two fallible steps (resource lookup,
    /// then disk write) are independently reachable without touching the
    /// environment at all, which is what the two tests below do.
    ///
    /// First ensures the resource is actually registered ([`bundle`], which
    /// this and every other test in this module needs regardless), so a
    /// lookup failure here is unambiguously about the bogus path, not about
    /// registration never having happened.
    #[test]
    fn materialize_with_an_unresolvable_resource_path_returns_a_typed_lookup_error() {
        let _ = bundle();

        let bogus = Face {
            family: "Bogus",
            weight: 0,
            resource_path: "/dev/hop/Launcher/fonts/does-not-exist.ttf",
        };
        let dir = tempfile::tempdir().unwrap_or_else(|err| panic!("tempdir: {err}"));

        match materialize(&bogus, dir.path()) {
            Err(FontsError::Lookup { path, .. }) => {
                assert_eq!(path, bogus.resource_path);
            }
            Err(other) => panic!("expected FontsError::Lookup, got a different variant: {other}"),
            Ok(path) => panic!(
                "expected materialize to fail for an unresolvable resource path, got Ok({})",
                path.display()
            ),
        }
    }

    /// [`materialize`]'s second fallible step — the disk write — must fail
    /// the same typed way when the destination directory does not exist,
    /// rather than panicking on the underlying `std::fs::write` error or
    /// silently reporting success.
    #[test]
    fn materialize_into_a_nonexistent_directory_returns_a_typed_materialize_error() {
        let _ = bundle();

        let face = FACES[0];
        let nonexistent_dir =
            std::env::temp_dir().join("hop-gtk-fonts-test-materialize-into-missing-dir");

        match materialize(&face, &nonexistent_dir) {
            Err(FontsError::Materialize { path, dest, .. }) => {
                assert_eq!(path, face.resource_path);
                assert_eq!(dest, nonexistent_dir.join(face.file_name()));
            }
            Err(other) => {
                panic!("expected FontsError::Materialize, got a different variant: {other}")
            }
            Ok(path) => panic!(
                "expected materialize to fail when the destination directory does not exist, \
                 got Ok({})",
                path.display()
            ),
        }
    }

    /// [`register_with_fontconfig`] must return `Ok(())` for a real
    /// directory holding real, materialized `.ttf` files — the happy path
    /// [`init`] itself already depends on (a failure here would make every
    /// other test in this module fail too, since [`bundle`] propagates it),
    /// exercised here directly and by name, rather than only indirectly
    /// through [`bundle`]'s own success.
    ///
    /// Registering the same directory with fontconfig a second time (this
    /// test's own call, after [`bundle`]'s own call already registered it
    /// once as a side effect of computing [`BUNDLE`]) is not documented
    /// anywhere as unsafe or disallowed — fontconfig's application-font set
    /// is additive, not keyed by directory, so re-adding one is at worst a
    /// harmless duplicate entry, not a hazard this test needs to route
    /// around.
    #[test]
    fn register_with_fontconfig_succeeds_for_the_materialized_bundle_directory() {
        let bundle = bundle().unwrap_or_else(|err| panic!("bundle() returned an error: {err}"));

        register_with_fontconfig(bundle.dir()).unwrap_or_else(|err| {
            panic!(
                "register_with_fontconfig failed for the real materialized directory {}: {err}",
                bundle.dir().display()
            )
        });
    }

    /// [`FontsError::FontconfigPath`] must actually be the type a failure
    /// comes back as when the directory path cannot be represented as a C
    /// string — the one input [`register_with_fontconfig`] can refuse
    /// before ever reaching the FFI call. A real filesystem path can never
    /// contain an interior NUL byte (see [`FontsError::FontconfigPath`]'s
    /// own doc comment for why), so this test constructs the NUL-containing
    /// [`PathBuf`] directly rather than trying to create one on disk — the
    /// same "exercise the typed refusal without needing an unreachable
    /// real-world condition" approach [`materialize_with_an_unresolvable_resource_path_returns_a_typed_lookup_error`]
    /// takes for its own bogus [`Face`].
    #[test]
    fn register_with_fontconfig_rejects_a_path_with_an_interior_nul_byte() {
        let bogus = PathBuf::from("/tmp/hop-gtk-fonts-test-\0-nul");

        match register_with_fontconfig(&bogus) {
            Err(FontsError::FontconfigPath { dir }) => {
                assert_eq!(dir, bogus);
            }
            Err(other) => {
                panic!("expected FontsError::FontconfigPath, got a different variant: {other}")
            }
            Ok(()) => panic!(
                "expected register_with_fontconfig to refuse a path with an interior NUL byte"
            ),
        }
    }
}
