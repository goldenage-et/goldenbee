//! Bounded publisher-side commands. No candidate is run during verification.

use crate::InstallError;
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Owned(Child);

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = killpg(Pid::from_raw(self.0.id() as i32), Signal::SIGKILL);
        let _ = self.0.wait();
    }
}

pub(super) fn run(command: &mut Command, max_bytes: u64) -> Result<Output, InstallError> {
    let mut child = Owned(
        command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| failure("command could not start"))?,
    );
    let (sender, receiver) = mpsc::channel();
    let streams: Vec<(bool, Box<dyn Read + Send>)> = vec![
        (true, Box::new(child.0.stdout.take().expect("piped stdout"))),
        (
            false,
            Box::new(child.0.stderr.take().expect("piped stderr")),
        ),
    ];
    for (stdout, stream) in streams {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stream.take(max_bytes + 1).read_to_end(&mut bytes);
            let _ = sender.send((stdout, result, bytes));
        });
    }
    drop(sender);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut stdout = None;
    let mut stderr = None;
    loop {
        while let Ok((is_stdout, result, bytes)) = receiver.try_recv() {
            result.map_err(|_| failure("command output is unreadable"))?;
            if bytes.len() as u64 > max_bytes {
                return Err(failure("command output exceeded its bound"));
            }
            if is_stdout {
                stdout = Some(bytes);
            } else {
                stderr = Some(bytes);
            }
        }
        if let Some(status) = child
            .0
            .try_wait()
            .map_err(|_| failure("command wait failed"))?
        {
            if let (Some(stdout), Some(stderr)) = (&stdout, &stderr) {
                return Ok(Output {
                    status,
                    stdout: stdout.clone(),
                    stderr: stderr.clone(),
                });
            }
        }
        if Instant::now() >= deadline {
            return Err(failure("command exceeded its 20s deadline"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn failure(message: &str) -> InstallError {
    InstallError::new("release.command_failed", message)
}
