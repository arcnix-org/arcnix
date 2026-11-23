use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

use anyhow::{Context, Result};
use arcnix_core::local_desktop::Target;
use arcnix_core::terminal::Output;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Check before advertising a local terminal. portable-pty assumes the ConPTY
/// exports exist when it opens its first Windows pseudo console.
pub(crate) fn check_support(target: &Target) -> Result<()> {
    target.require_write()?;
    #[cfg(windows)]
    {
        use std::sync::OnceLock;

        use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
        use windows::core::{s, w};
        static CONPTY: OnceLock<bool> = OnceLock::new();
        let supported = CONPTY.get_or_init(|| unsafe {
            GetModuleHandleW(w!("kernel32.dll")).is_ok_and(|kernel| {
                [
                    s!("CreatePseudoConsole"),
                    s!("ResizePseudoConsole"),
                    s!("ClosePseudoConsole"),
                ]
                .into_iter()
                .all(|symbol| GetProcAddress(kernel, symbol).is_some())
            })
        });
        anyhow::ensure!(
            *supported,
            "This Windows version does not support local terminals."
        );
        if matches!(target, Target::Native) {
            arcnix_core::local_desktop::windows_host::powershell()?;
        }
    }
    Ok(())
}

pub(super) struct LocalTerminal {
    child: Box<dyn Child + Send + Sync>,
    input: SyncSender<Vec<u8>>,
    output: Receiver<std::io::Result<Vec<u8>>>,
    master: Option<Box<dyn MasterPty + Send>>,
    child_exit: Option<i32>,
    remainder: Vec<u8>,
}

impl LocalTerminal {
    pub(super) fn open(
        target: &Target,
        directory: Option<&str>,
        columns: u32,
        rows: u32,
    ) -> Result<Self> {
        check_support(target)?;
        if let Some(path) = directory {
            arcnix_core::terminal::validate_directory(path)?;
        }
        let size = size(columns, rows);
        let pair = native_pty_system().openpty(size)?;
        let command = match target {
            #[cfg(windows)]
            Target::Wsl(name) => {
                arcnix_core::wsl::profile_host(name)?;
                let mut cmd = CommandBuilder::new(arcnix_core::wsl::launcher_path()?);
                cmd.args(["--distribution", name, "--cd", directory.unwrap_or("~")]);
                cmd
            }
            #[cfg(windows)]
            Target::Native | Target::ReadOnlyHost => {
                use arcnix_core::local_desktop::windows_host;
                let mut cmd = CommandBuilder::new(windows_host::powershell()?);
                cmd.args(["-NoLogo", "-NoProfile"]);
                let home = target.home()?;
                cmd.cwd(windows_host::physical(
                    directory.filter(|p| *p != "/").unwrap_or(&home),
                )?);
                cmd
            }
            #[cfg(unix)]
            Target::Native | Target::ReadOnlyHost => {
                let mut cmd = CommandBuilder::new_default_prog();
                let cwd = directory
                    .map(std::path::PathBuf::from)
                    .or_else(std::env::home_dir)
                    .context("Missing home directory")?;
                anyhow::ensure!(cwd.is_dir(), "Terminal directory does not exist");
                cmd.cwd(cwd);
                cmd.env("TERM", "xterm-256color");
                cmd
            }
        };
        let mut reader = pair.master.try_clone_reader()?;
        let mut writer = pair.master.take_writer()?;
        let child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        let (tx, output) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            let mut buf = [0; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(Ok(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::BrokenPipe
                            || (cfg!(unix) && e.raw_os_error() == Some(5)) =>
                    {
                        break;
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });
        let (input, rx) = mpsc::sync_channel::<Vec<u8>>(64);
        std::thread::spawn(move || {
            for data in rx {
                if writer.write_all(&data).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input,
            output,
            master: Some(pair.master),
            child_exit: None,
            remainder: Vec::new(),
        })
    }
}

fn size(columns: u32, rows: u32) -> PtySize {
    PtySize {
        cols: columns.clamp(2, 500) as u16,
        rows: rows.clamp(1, 300) as u16,
        ..Default::default()
    }
}

impl super::terminal_worker::TerminalIo for LocalTerminal {
    fn input(&mut self, bytes: Vec<u8>) -> Result<()> {
        self.input
            .try_send(bytes)
            .context("Terminal input queue is unavailable")
    }
    fn resize(&mut self, columns: u32, rows: u32) -> Result<()> {
        if let Some(master) = &self.master {
            master.resize(size(columns, rows))?;
        }
        Ok(())
    }
    fn poll(&mut self, budget: usize) -> Result<Output> {
        if self.child_exit.is_none()
            && let Some(status) = self.child.try_wait()?
        {
            self.child_exit = Some(status.exit_code() as i32);
            // Closing ConPTY flushes its final screen update. Keep draining the
            // bounded reader queue while Windows completes the close.
            if let Some(master) = self.master.take() {
                std::thread::spawn(move || drop(master));
            }
        }
        let mut exit_code = None;
        if self.remainder.is_empty() && budget > 0 {
            match self.output.try_recv() {
                Ok(data) => self.remainder = data?,
                Err(TryRecvError::Disconnected) => exit_code = self.child_exit,
                Err(TryRecvError::Empty) => {}
            }
        }
        let n = budget.min(self.remainder.len());
        Ok(Output {
            bytes: self.remainder.drain(..n).collect(),
            exit_code,
        })
    }
}

impl Drop for LocalTerminal {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::terminal_worker::TerminalIo;
    use super::*;

    #[test]
    fn native_terminal_uses_requested_directory_and_size() -> Result<()> {
        use std::time::{Duration, Instant};
        let folder = std::env::temp_dir().join(format!("arcnix-pty-{}", arcnix_core::new_id()));
        std::fs::create_dir(&folder)?;
        let result = (|| -> Result<()> {
            let mut pty = LocalTerminal::open(&Target::Native, folder.to_str(), 80, 24)?;
            let deadline = Instant::now() + Duration::from_secs(15);
            pty.resize(96, 32)?;
            // A login script can reset the terminal and discard early typeahead.
            // Repeat the harmless probe until the interactive shell handles it.
            let mut next_probe = Instant::now();
            while pty.poll(65536)?.exit_code.is_none() {
                let now = Instant::now();
                anyhow::ensure!(now < deadline, "Native terminal command did not finish");
                if now >= next_probe && !folder.join("size").exists() {
                    pty.input(
                        b"HISTFILE=/dev/null; pwd -P > cwd; stty size > size; exit\r".to_vec(),
                    )?;
                    next_probe = now + Duration::from_millis(500);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                std::fs::read_to_string(folder.join("cwd"))?.trim_end(),
                folder.canonicalize()?.to_str().unwrap()
            );
            assert_eq!(
                std::fs::read_to_string(folder.join("size"))?.trim(),
                "32 96"
            );
            Ok(())
        })();
        std::fs::remove_dir_all(folder)?;
        result
    }
}
