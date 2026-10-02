//! Running a helper program with a deadline and a cap on what it prints: the
//! `gh` transport's calls and the desktop's update steps share it.

use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// `cmd`'s output, or `Ok(None)` when it outlived `timeout` and was killed
/// together with its process group, so a descendant holding its pipes dies
/// too. stdout (when `capture_stdout`; otherwise it goes nowhere) and stderr
/// each keep at most `max_bytes`; the rest is read and dropped, so the child
/// never stalls on a full pipe. The wait is on a channel, not a polling loop:
/// a command returns the moment it finishes.
pub fn run_bounded(
    cmd: &mut Command,
    timeout: Duration,
    max_bytes: usize,
    capture_stdout: bool,
) -> io::Result<Option<Output>> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(if capture_stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (done, finished) = mpsc::channel();
    // Both pipes are read at once (one would fill while the other is read),
    // and the child is reaped here, so the one wait below covers it all.
    std::thread::spawn(move || {
        let errors = std::thread::spawn(move || read_capped(stderr, max_bytes));
        let out = read_capped(stdout, max_bytes);
        let err = errors
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("stderr reader panicked")));
        let status = child.wait();
        let _ = done.send((status, out, err));
    });
    match finished.recv_timeout(timeout) {
        Ok((status, stdout, stderr)) => Ok(Some(Output {
            status: status?,
            stdout: stdout?,
            stderr: stderr?,
        })),
        Err(_) => {
            kill_group(pid);
            Ok(None)
        }
    }
}

fn read_capped(pipe: Option<impl Read>, max_bytes: usize) -> io::Result<Vec<u8>> {
    let Some(mut pipe) = pipe else {
        return Ok(Vec::new());
    };
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = pipe.read(&mut buffer)?;
        if read == 0 {
            return Ok(captured);
        }
        let room = max_bytes.saturating_sub(captured.len());
        captured.extend_from_slice(&buffer[..read.min(room)]);
    }
}

/// SIGKILL the process group `run_bounded` started, by the system `kill`
/// (no libc dependency), falling back to the one on `PATH`.
fn kill_group(pid: u32) {
    let group = format!("-{pid}");
    for kill in ["/bin/kill", "kill"] {
        let killed = Command::new(kill)
            .args(["-KILL", "--", &group])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if killed {
            return;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", script]);
        cmd
    }

    #[test]
    fn output_is_capped_and_the_rest_is_drained() {
        let output = run_bounded(
            &mut sh("i=0; while [ $i -lt 6000 ]; do printf x; i=$((i + 1)); done"),
            Duration::from_secs(5),
            4096,
            true,
        )
        .unwrap()
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 4096);
    }

    #[test]
    fn output_is_kept_when_the_pipes_close_at_different_times() {
        let output = run_bounded(
            &mut sh("printf early; exec 1>&-; sleep 0.1; printf late >&2"),
            Duration::from_secs(5),
            4096,
            true,
        )
        .unwrap()
        .unwrap();
        assert_eq!(output.stdout, b"early");
        assert_eq!(output.stderr, b"late");
    }

    #[test]
    fn a_descendant_holding_the_pipes_is_killed_at_the_deadline() {
        let result = run_bounded(
            &mut sh("sleep 10 & exit 0"),
            Duration::from_millis(100),
            4096,
            true,
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn stdout_can_go_nowhere() {
        let output = run_bounded(
            &mut sh("printf secret"),
            Duration::from_secs(5),
            4096,
            false,
        )
        .unwrap()
        .unwrap();
        assert!(output.stdout.is_empty());
    }
}
