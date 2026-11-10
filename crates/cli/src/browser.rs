//! Purpose: Hand a printed URL to the operating system's default browser opener.
//!
//! # Contract
//! - **Serves:** `loongagent web` and `loongagent reverse`, which print a URL a person must open.
//! - **Design:** `docs/design/surfaces.md` §4.
//! - **Upstream:** `web::web` and `reverse::run_portal`.
//! - **Downstream:** the OS opener (`xdg-open`, `open`, `start`) as a detached child process.
//! - **Verify:** `cargo test -p loongagent browser` · `./scripts/check.sh`

use std::process::{Command, Stdio};

/// Purpose: Name the program that opens `url` in the person's default browser, per platform.
/// Serves: `open_in_browser`.
/// Design: `docs/design/surfaces.md` §4.
/// Up: `open_in_browser` only.
/// Down: nothing; it returns a process description and touches no state.
/// Verify: `cargo test -p loongagent browser`.
fn opener(url: &str) -> (&'static str, Vec<&str>) {
    if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(target_os = "windows") {
        // `start` is a cmd builtin, and its first argument is the window title.
        ("cmd", vec!["/c", "start", "", url])
    } else {
        ("xdg-open", vec![url])
    }
}

/// Purpose: Open `url` in the default browser without waiting for it; if no opener exists, say
/// so and leave the printed URL as the way in.
/// Serves: `loongagent web` and `loongagent reverse`, so one command both serves and shows the page.
/// Design: `docs/design/surfaces.md` §4.
/// Up: `web::web` and `reverse::run_portal`, right after they print the URL.
/// Down: `opener`, then `Command::spawn` and a thread that reaps the child.
/// Verify: `cargo test -p loongagent browser` covers the command shape; the launch itself is
/// checked by hand, because a test must not open a browser.
pub(crate) fn open_in_browser(url: &str) {
    let (program, arguments) = opener(url);
    // Quiet and detached: the browser does not belong to us, and a headless machine's
    // complaint about a missing display would only bury the URL printed above it.
    let child = Command::new(program)
        .args(&arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match child {
        // Wait on a thread: an exited child we never reap stays a zombie, and waiting
        // here could block for as long as the browser runs.
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(error) => eprintln!("[open] {program} did not start ({error}); open the URL above."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: The opener names a program and passes the URL through unchanged.
    #[test]
    fn the_opener_names_a_program_and_passes_the_url() {
        let url = "http://127.0.0.1:7391/reverse#token=abc";
        let (program, arguments) = opener(url);
        assert!(!program.is_empty(), "the opener must name a program");
        assert_eq!(
            arguments.last().copied(),
            Some(url),
            "the URL goes through whole: {arguments:?}"
        );
        if cfg!(target_os = "windows") {
            assert_eq!(program, "cmd", "Windows opens URLs through cmd's start");
        } else if cfg!(target_os = "macos") {
            assert_eq!(program, "open", "macOS opens URLs through open");
        } else {
            assert_eq!(program, "xdg-open", "Unix opens URLs through xdg-open");
        }
    }
}
