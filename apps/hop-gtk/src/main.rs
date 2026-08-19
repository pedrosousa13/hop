//! `hop-gtk` — the GTK4 + libadwaita launcher window's entry point. Thin by
//! design: see `hop_gtk::lib`'s doc comment for why the real logic lives in
//! the library half of this crate instead.

use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    hop_gtk::app::run(env::args().skip(1))
}
