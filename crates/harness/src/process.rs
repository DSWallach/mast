//! Owned agent processes. Non-Windows builds retain Tokio's command and child.
//! Windows uses native creation-time Job Object membership, never an implicit shell.
#[cfg(not(windows))]
pub use std::process::Stdio;
#[cfg(not(windows))]
pub use tokio::process::{Child, ChildStdin, ChildStdout, Command};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Child, ChildStdin, ChildStdout, Command, Stdio};

#[cfg(unix)]
fn child_file_limit(soft: libc::rlim_t, hard: libc::rlim_t) -> libc::rlim_t {
    soft.max(hard.min(8192))
}

/// Configure only the child. The hook uses async-signal-safe libc calls after fork.
#[cfg(unix)]
pub(crate) fn raise_child_file_limit(command: &mut Command) {
    // SAFETY: the hook only calls getrlimit, setrlimit and write, without
    // allocation or locks, and always permits the child to continue.
    unsafe {
        command.pre_exec(|| {
            let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
                let message = b"debug: could not read child open-file limit\n";
                libc::write(libc::STDERR_FILENO, message.as_ptr().cast(), message.len());
            } else {
                limit.rlim_cur = child_file_limit(limit.rlim_cur, limit.rlim_max);
                if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                    let message = b"debug: could not raise child open-file limit\n";
                    libc::write(libc::STDERR_FILENO, message.as_ptr().cast(), message.len());
                }
            }
            Ok(())
        });
    }
}

#[cfg(unix)]
pub(crate) fn signal_target(child: &Child) -> Option<i32> {
    let pid = child.id()? as i32;
    // ACP children lead a private group; other harnesses retain pid signaling.
    // SAFETY: getpgid only inspects the owned, unreaped child.
    Some(if unsafe { libc::getpgid(pid) } == pid {
        -pid
    } else {
        pid
    })
}
#[cfg(windows)]
pub(crate) fn signal_target(child: &Child) -> Option<std::sync::Arc<crate::windows_process::Job>> {
    Some(child.job.clone())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn child_file_limit_respects_hard_limit() {
        assert_eq!(child_file_limit(256, 1024), 1024);
        assert_eq!(child_file_limit(256, 16384), 8192);
        assert_eq!(child_file_limit(16384, 32768), 16384);
    }

    #[tokio::test]
    async fn child_receives_file_limit_without_changing_parent() {
        let mut before = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut before) }, 0);
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "ulimit -n"]).stdout(Stdio::piped());
        raise_child_file_limit(&mut command);
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        let actual: libc::rlim_t = String::from_utf8(output.stdout).unwrap().trim().parse().unwrap();
        assert_eq!(actual, child_file_limit(before.rlim_cur, before.rlim_max));
        let mut after = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut after) }, 0);
        assert_eq!(after.rlim_cur, before.rlim_cur);
    }

    #[tokio::test]
    async fn unix_launch_retains_tokio_types_and_output() {
        let mut command: tokio::process::Command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("--list")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        let child: tokio::process::Child = command.spawn().unwrap();
        let result = child.wait_with_output().await.unwrap();
        assert!(result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stdout)
                .contains("unix_launch_retains_tokio_types_and_output")
        );
        let result = command.output().await.unwrap();
        assert!(result.status.success());
        assert!(!result.stdout.is_empty());
    }
}
