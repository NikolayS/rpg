//! File I/O, buffer, and utility command handlers for Samo.
//!
//! Implements the low-level side-effects for the I/O family of backslash
//! commands: `\i`, `\ir`, `\o`, `\w`, `\e`, `\!`, `\cd`, `\echo`, `\qecho`,
//! `\warn`, `\encoding`, and `\password`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;

use tokio_postgres::Client;

use crate::connection::ConnParams;
use crate::repl::{ReplSettings, TxState};

// ---------------------------------------------------------------------------
// Include file
// ---------------------------------------------------------------------------

/// Execute every statement in a SQL file, just as `\i` does in psql.
///
/// Lines are processed via the shared [`crate::repl::exec_lines`] helper,
/// which handles both SQL accumulation and backslash meta-commands (including
/// `\if` / `\elif` / `\else` / `\endif` for conditional execution).
///
/// `tx` is the caller's transaction state and is updated in-place so that
/// transaction context is inherited across `\i` / `\ir` inclusions.
///
/// Returns 0 on success, 1 if the file cannot be read or any statement
/// produces a SQL error.
///
/// # Note on boxing
/// This function returns a [`Pin<Box<dyn Future>>`] rather than using
/// `async fn` because `include_file` → `exec_lines` → `dispatch_meta` →
/// `dispatch_io` → `include_file` forms a recursive async call cycle.
/// `Box::pin` breaks the cycle by giving the future an explicit heap
/// allocation and a fixed size.
pub fn include_file<'a>(
    client: &'a Client,
    path: &'a str,
    settings: &'a mut ReplSettings,
    tx: &'a mut TxState,
    params: &'a ConnParams,
) -> Pin<Box<dyn std::future::Future<Output = i32> + 'a>> {
    Box::pin(async move {
        let content = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                // Match psql's error format: "{path}: {os error string}"
                // Rust appends " (os error N)" to the OS message; strip it.
                let full = e.to_string();
                let msg = full
                    .find(" (os error ")
                    .map_or(full.as_str(), |pos| &full[..pos]);
                eprintln!("{path}: {msg}");
                return 1;
            }
        };

        // Save and update current_file so that nested \ir commands resolve
        // paths relative to the directory of this file.
        let prev_file = settings.current_file.clone();
        settings.current_file = Some(path.to_owned());

        let start_depth = settings.cond.depth();
        let exit_code = crate::repl::exec_lines(
            client,
            content.lines().map(str::to_owned),
            settings,
            params,
            tx,
        )
        .await;

        // Restore the previous current_file so callers see the right value
        // after this include returns.
        settings.current_file = prev_file;

        let end_depth = settings.cond.depth();
        if end_depth > start_depth {
            eprintln!(
                "samo: warning: {} unterminated \\if block(s) at end of file \"{path}\"",
                end_depth - start_depth
            );
        }

        exit_code
    })
}

// ---------------------------------------------------------------------------
// Path resolution for \ir
// ---------------------------------------------------------------------------

/// Resolve a path for `\ir` (include-relative).
///
/// If `raw` is absolute, returns it unchanged.  Otherwise, resolves it
/// relative to the directory of `current_file` when one is set; if
/// `current_file` is `None` the path is returned as-is (equivalent to
/// CWD-relative, matching `\i` behaviour).
pub fn resolve_relative_path(raw: &str, current_file: Option<&str>) -> String {
    let raw_path = Path::new(raw);
    if raw_path.is_absolute() {
        return raw.to_owned();
    }
    if let Some(cf) = current_file {
        let base: PathBuf = Path::new(cf)
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        return base.join(raw_path).to_string_lossy().into_owned();
    }
    raw.to_owned()
}

// ---------------------------------------------------------------------------
// Output redirection
// ---------------------------------------------------------------------------

/// Open a file for output redirection (`\o file`).
///
/// Returns `Ok(Some(writer))` on success. Returns `Ok(None)` when `path` is
/// `None`, signalling that the caller should restore stdout.
///
/// # Errors
/// Returns `Err(message)` if the file cannot be opened for writing.
pub fn open_output(path: Option<&str>) -> Result<Option<Box<dyn Write>>, String> {
    match path {
        None => Ok(None),
        Some(p) => {
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(p)
                .map_err(|e| format!("\\o: could not open \"{p}\": {e}"))?;
            Ok(Some(Box::new(file)))
        }
    }
}

// ---------------------------------------------------------------------------
// Write buffer to file
// ---------------------------------------------------------------------------

/// Write the current query buffer contents to a file (`\w file`).
///
/// # Errors
/// Returns `Err(message)` if the file cannot be created or written to.
pub fn write_buffer(buf: &str, path: &str) -> Result<(), String> {
    let mut file =
        File::create(path).map_err(|e| format!("\\w: could not create \"{path}\": {e}"))?;
    file.write_all(buf.as_bytes())
        .map_err(|e| format!("\\w: write failed for \"{path}\": {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Editor
// ---------------------------------------------------------------------------

/// Open `$EDITOR` (or `$VISUAL`, falling back to `vi`) with the given
/// content, and return the content after the editor exits.
///
/// If `file` is provided the named file is opened directly (and the caller
/// should pass its current contents as `content`). Otherwise a temporary file
/// is created, pre-populated with `content`.
///
/// If `line` is provided, an attempt is made to pass `+line` to the editor
/// as a positional argument so the editor opens at that line number.
///
/// # Errors
/// Returns `Err(message)` if the editor cannot be launched or the temporary
/// file cannot be read back.
pub fn edit(content: &str, file: Option<&str>, line: Option<usize>) -> Result<String, String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_owned());

    // Determine the file path to edit.
    let (path, is_temp) = if let Some(f) = file {
        (f.to_owned(), false)
    } else {
        let tmp = temp_file_path();
        // Write buffer content to the temp file.
        std::fs::write(&tmp, content)
            .map_err(|e| format!("\\e: could not create temp file: {e}"))?;
        (tmp, true)
    };

    // Build the editor command.
    let mut cmd = Command::new(&editor);
    if let Some(n) = line {
        cmd.arg(format!("+{n}"));
    }
    cmd.arg(&path);

    let status = cmd
        .status()
        .map_err(|e| format!("\\e: could not launch editor \"{editor}\": {e}"))?;

    if !status.success() {
        eprintln!("\\e: editor exited with status {status}");
    }

    // Read back the (possibly modified) file.
    let result = std::fs::read_to_string(&path)
        .map_err(|e| format!("\\e: could not read back \"{path}\": {e}"))?;

    // Clean up temp file.
    if is_temp {
        let _ = std::fs::remove_file(&path);
    }

    Ok(result)
}

/// Return a path for a temporary file.
fn temp_file_path() -> String {
    let pid = std::process::id();
    let dir = std::env::temp_dir();
    dir.join(format!("samo_edit_{pid}.sql"))
        .to_string_lossy()
        .into_owned()
}

// ---------------------------------------------------------------------------
// Shell command
// ---------------------------------------------------------------------------

/// Execute a shell command (`\!`).
///
/// If `cmd` is `None`, spawns an interactive shell (`$SHELL` or `/bin/sh`).
/// The command is passed to the shell via `-c`. Standard I/O is inherited
/// from the parent process so output goes directly to the terminal.
///
/// Returns the exit code of the child process, or 1 if it could not be
/// launched.
pub fn shell_command(cmd: Option<&str>) -> i32 {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());

    let status = if let Some(c) = cmd {
        Command::new(&shell).arg("-c").arg(c).status()
    } else {
        Command::new(&shell).status()
    };

    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("\\!: could not launch shell \"{shell}\": {e}");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// Change directory
// ---------------------------------------------------------------------------

/// Change the working directory (`\cd`).
///
/// If `dir` is `None`, changes to the user's home directory (matching psql
/// behaviour). Uses `std::env::set_current_dir` — the change persists for
/// the duration of the process.
///
/// # Errors
/// Returns `Err(message)` if the directory does not exist or is not
/// accessible.
pub fn change_dir(dir: Option<&str>) -> Result<(), String> {
    let target: std::path::PathBuf = match dir {
        Some(d) => Path::new(d).to_path_buf(),
        None => {
            dirs::home_dir().ok_or_else(|| "\\cd: could not determine home directory".to_owned())?
        }
    };

    std::env::set_current_dir(&target).map_err(|e| format!("\\cd: {}: {e}", target.display()))
}

// ---------------------------------------------------------------------------
// Client encoding (stub)
// ---------------------------------------------------------------------------

/// Show or set the client encoding (`\encoding`).
///
/// Full encoding negotiation with the server is not yet implemented. When
/// called with no argument, prints `UTF8` (the only encoding currently
/// supported). When called with an argument, prints a notice that the feature
/// is limited.
pub fn encoding(enc: Option<&str>) {
    match enc {
        None => println!("UTF8"),
        Some(e) => {
            if e.to_uppercase() == "UTF8" || e.to_uppercase() == "UTF-8" {
                println!("UTF8");
            } else {
                eprintln!("\\encoding: encoding \"{e}\" is not yet supported (only UTF8)");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::io::Read;

    // -- shell_command -------------------------------------------------------

    #[test]
    fn shell_echo_returns_zero() {
        let code = shell_command(Some("echo hello"));
        assert_eq!(code, 0, "shell echo should return exit code 0");
    }

    #[test]
    fn shell_false_returns_nonzero() {
        let code = shell_command(Some("false"));
        assert_ne!(code, 0, "shell false should return a non-zero exit code");
    }

    // -- change_dir ----------------------------------------------------------

    #[test]
    fn change_dir_to_tmp() {
        let tmp = std::env::temp_dir();
        let result = change_dir(Some(tmp.to_str().expect("tmp path is valid UTF-8")));
        assert!(result.is_ok(), "chdir to /tmp should succeed: {result:?}");
        // Restore to original directory — best effort.
        let _ = change_dir(Some("."));
    }

    #[test]
    fn change_dir_nonexistent_fails() {
        let result = change_dir(Some("/this/path/does/not/exist/samo_test"));
        assert!(result.is_err(), "chdir to nonexistent path should fail");
    }

    // -- write_buffer --------------------------------------------------------

    #[test]
    fn write_buffer_roundtrip() {
        let dir = std::env::temp_dir();
        let path = dir.join("samo_test_write_buffer.sql");
        let path_str = path.to_str().expect("path is valid UTF-8");

        write_buffer("select 1;", path_str).expect("write_buffer should succeed");

        let mut file = File::open(&path).expect("file should exist after write_buffer");
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .expect("reading back file should succeed");

        assert_eq!(contents, "select 1;");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_buffer_invalid_path_fails() {
        let result = write_buffer("select 1;", "/no/such/directory/samo_test.sql");
        assert!(result.is_err(), "write to invalid path should fail");
    }

    // -- open_output ---------------------------------------------------------

    #[test]
    fn open_output_none_returns_none() {
        let result = open_output(None).expect("open_output(None) should be Ok");
        assert!(result.is_none(), "open_output(None) should return None");
    }

    #[test]
    fn open_output_path_creates_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("samo_test_open_output.txt");
        let path_str = path.to_str().expect("path is valid UTF-8");

        let result = open_output(Some(path_str)).expect("open_output should succeed");
        assert!(
            result.is_some(),
            "open_output with a path should return Some"
        );

        let _ = std::fs::remove_file(&path);
    }

    // -- edit (temp file creation and read-back) ----------------------------

    #[test]
    #[serial]
    fn edit_creates_and_reads_temp_file() {
        // Use a no-op editor (cat or true) that doesn't modify the file.
        // We override $EDITOR to `cat` so the test is non-interactive.
        // `cat file` prints the file and exits 0 without modification.
        // Clear $VISUAL so it doesn't override $EDITOR.
        std::env::set_var("EDITOR", "cat");
        std::env::remove_var("VISUAL");
        let content = "select 42;";
        let result = edit(content, None, None);
        // cat should succeed and return the original content unchanged.
        assert!(result.is_ok(), "edit with cat should succeed: {result:?}");
        assert_eq!(result.unwrap(), content);
    }

    // -- resolve_relative_path -----------------------------------------------

    #[test]
    fn resolve_relative_no_current_file_returns_as_is() {
        let result = resolve_relative_path("foo.sql", None);
        assert_eq!(result, "foo.sql");
    }

    #[test]
    fn resolve_relative_absolute_path_unchanged() {
        let result = resolve_relative_path("/abs/path/foo.sql", Some("/some/script.sql"));
        assert_eq!(result, "/abs/path/foo.sql");
    }

    #[test]
    fn resolve_relative_relative_to_current_file_dir() {
        let result = resolve_relative_path("bar.sql", Some("/scripts/main.sql"));
        assert_eq!(result, "/scripts/bar.sql");
    }

    #[test]
    fn resolve_relative_current_file_in_root() {
        // When the current file is at the root, parent is "/".
        let result = resolve_relative_path("foo.sql", Some("/root.sql"));
        assert_eq!(result, "/foo.sql");
    }

    #[test]
    fn resolve_relative_subdirectory() {
        let result = resolve_relative_path("sub/child.sql", Some("/scripts/main.sql"));
        assert_eq!(result, "/scripts/sub/child.sql");
    }
}
