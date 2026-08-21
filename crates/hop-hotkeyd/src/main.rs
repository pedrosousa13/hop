//! Entry point. Loads the `[hotkey]` bindings from `config.toml` and either
//! runs the grab loop or — when there is nothing to grab, for any reason
//! issue #234's criterion 2 names — logs why and exits 0 as a no-op.
//!
//! The no-op posture is this binary's whole error-handling policy, and it
//! is deliberately narrower than its siblings': `hopd` refuses to start on
//! a malformed config and `hop-gtk` refuses on a malformed keymap (both
//! because silently-wrong behavior is worse than absence), while
//! `hop-hotkeyd` logs and exits cleanly. An optional resident agent whose
//! absence is a fully-supported state must not turn "no hotkeys
//! configured" into a crash-looping systemd unit; the log line is what
//! keeps the no-op from being silent. See `config.rs`'s module doc for the
//! full argument.

use std::process::ExitCode;

use hop_hotkeyd::config;

fn main() -> ExitCode {
    let Some(path) = config::config_path() else {
        eprintln!(
            "hop-hotkeyd: neither XDG_CONFIG_HOME nor HOME is set; \
             no config to read, nothing to grab"
        );
        return ExitCode::SUCCESS;
    };

    match config::load(&path) {
        Ok(None) => {
            eprintln!(
                "hop-hotkeyd: no [hotkey] section in {}; nothing to grab",
                path.display()
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("hop-hotkeyd: {err}; running as a no-op");
            ExitCode::SUCCESS
        }
        Ok(Some(hotkeys)) => hop_hotkeyd::run::run(&hotkeys.toggle),
    }
}
