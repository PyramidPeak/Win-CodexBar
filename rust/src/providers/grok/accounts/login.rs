use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{SavedLogin, read_login};

static CANCEL: AtomicBool = AtomicBool::new(false);

const AUTH_OVERRIDES: [&str; 3] = [
    "XAI_API_KEY",
    "GROK_OAUTH_TOKEN",
    "CODEXBAR_GROK_OAUTH_TOKEN",
];

pub fn cancel_login() {
    CANCEL.store(true, Ordering::SeqCst);
}

pub fn begin_login() {
    CANCEL.store(false, Ordering::SeqCst);
}

pub fn executable() -> io::Result<PathBuf> {
    if let Some(home) = dirs::home_dir() {
        let native = home.join(if cfg!(windows) {
            ".grok/bin/grok.exe"
        } else {
            ".grok/bin/grok"
        });
        if native.is_file() {
            return Ok(native);
        }
    }
    which::which("grok").map_err(|_| {
        io::Error::other("Grok CLI was not found. Install Grok Build, then try again.")
    })
}

fn command(executable: &Path, home: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .env("GROK_HOME", home)
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for key in AUTH_OVERRIDES {
        command.env_remove(key);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

struct LoginDirectory {
    root: PathBuf,
    path: PathBuf,
}

impl Drop for LoginDirectory {
    fn drop(&mut self) {
        if let (Ok(root), Ok(path)) = (self.root.canonicalize(), self.path.canonicalize())
            && path.parent() == Some(root.as_path())
        {
            let _cleanup = std::fs::remove_dir_all(path);
        }
    }
}

fn login_root() -> io::Result<PathBuf> {
    dirs::config_dir()
        .map(|dir| dir.join("CodexBar/grok-accounts/logins"))
        .ok_or_else(|| io::Error::other("Configuration directory not found."))
}

pub fn cleanup_abandoned_logins() -> io::Result<()> {
    let root = login_root()?;
    match std::fs::symlink_metadata(&root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let root = root.canonicalize()?;
    for entry in entries.flatten() {
        if uuid::Uuid::parse_str(&entry.file_name().to_string_lossy()).is_err() {
            continue;
        }
        let Ok(path) = entry.path().canonicalize() else {
            continue;
        };
        if path.parent() == Some(root.as_path()) {
            let _cleanup = std::fs::remove_dir_all(path);
        }
    }
    Ok(())
}

pub fn login() -> io::Result<SavedLogin> {
    let exe = executable()?;
    let root = login_root()?;
    std::fs::create_dir_all(&root)?;
    let dir = LoginDirectory {
        path: root.join(uuid::Uuid::new_v4().to_string()),
        root,
    };
    std::fs::create_dir(&dir.path)?;
    let mut child = command(&exe, &dir.path)
        .args(["login", "--oauth", "--leader-socket"])
        .arg(dir.path.join("leader.sock"))
        .spawn()
        .map_err(|e| io::Error::other(format!("Failed to start Grok login: {e}")))?;
    wait_for_login(&mut child, &dir.path, Duration::from_secs(300))
}

fn wait_for_login(child: &mut Child, home: &Path, timeout: Duration) -> io::Result<SavedLogin> {
    let start = Instant::now();
    loop {
        if let Some(login) = read_login(&home.join("auth.json"))? {
            let _kill = child.kill();
            let _reap = child.wait();
            return Ok(login);
        }
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(e) => {
                let _kill = child.kill();
                let _reap = child.wait();
                return Err(e);
            }
        };
        if let Some(status) = status {
            if !status.success() {
                return Err(io::Error::other(format!(
                    "Grok sign-in did not complete (exit code {}). Finish sign-in in your browser and try again.",
                    status.code().unwrap_or(-1)
                )));
            }
            return read_login(&home.join("auth.json"))?.ok_or_else(|| {
                io::Error::other(
                    "Grok did not save a SuperGrok login. Finish sign-in in your browser, then try again.",
                )
            });
        }
        let cancelled = CANCEL.load(Ordering::SeqCst);
        if cancelled || start.elapsed() > timeout {
            let _kill = child.kill();
            let _reap = child.wait();
            return Err(io::Error::other(if cancelled {
                "Grok sign-in cancelled."
            } else {
                "Grok sign-in timed out. Please try again."
            }));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_flag_can_be_reset() {
        begin_login();
        assert!(!CANCEL.load(Ordering::SeqCst));
        cancel_login();
        assert!(CANCEL.load(Ordering::SeqCst));
        begin_login();
        assert!(!CANCEL.load(Ordering::SeqCst));
    }
}
