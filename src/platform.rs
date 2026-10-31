//! Platform openers — the one place that knows how to hand a path, a
//! directory or a URL to the desktop (`open` / `xdg-open` / `cmd /c
//! start`). Every "open", "reveal in file manager", "open terminal" and
//! "open URL" action goes through here instead of carrying its own
//! `cfg(target_os)` ladder.

use std::io;
use std::path::Path;
use std::process::Command;

/// The desktop opener binary this build uses for [`open_path`] and
/// [`open_url`] (exposed for tests).
pub fn opener_command() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "cmd"
    } else {
        "xdg-open"
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn unsupported(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, format!("{what} is not supported on this platform"))
}

/// Open a file or directory with the OS default application.
pub fn open_path(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(path).spawn()?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open").arg(path).spawn()?;
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd").args(["/c", "start", ""]).arg(path).spawn()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = path;
        Err(unsupported("open"))
    }
}

/// Reveal a file in the platform file manager (Finder / Explorer; on
/// Linux the parent directory is opened, a no-op for a path without one).
pub fn reveal_path(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg("-R").arg(path).spawn()?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(parent) = path.parent() {
            Command::new("xdg-open").arg(parent).spawn()?;
        }
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("explorer").arg("/select,").arg(path).spawn()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = path;
        Err(unsupported("reveal"))
    }
}

/// Open a terminal window in `dir`. On Linux the first emulator that
/// launches wins: `x-terminal-emulator`, `gnome-terminal`, `konsole`,
/// `xterm`; `NotFound` when none does.
pub fn open_terminal(dir: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg("-a").arg("Terminal").arg(dir).spawn()?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let terminals = ["x-terminal-emulator", "gnome-terminal", "konsole", "xterm"];
        for term in &terminals {
            if Command::new(term).arg("--working-directory").arg(dir).spawn().is_ok() {
                return Ok(());
            }
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "no terminal emulator found"))
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd").args(["/c", "start", "cmd"]).current_dir(dir).spawn()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = dir;
        Err(unsupported("open terminal"))
    }
}

/// Open a URL in the default browser.
pub fn open_url(url: &str) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn()?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open").arg(url).spawn()?;
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd").args(["/c", "start", url]).spawn()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = url;
        Err(unsupported("open URL"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn opener_is_open_on_macos() {
        assert_eq!(opener_command(), "open");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn opener_is_xdg_open_on_linux() {
        assert_eq!(opener_command(), "xdg-open");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn opener_is_cmd_on_windows() {
        assert_eq!(opener_command(), "cmd");
    }
}
