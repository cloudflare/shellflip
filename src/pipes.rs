use libc::c_int;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};

pub(crate) enum PipeMode {
    ParentWrites,
    ChildWrites,
}

/// Create a pair of pipes.
/// The first element is the read side, and the second element is the write side.
/// `mode` determines whether the read or write end will be inherited by the child.
pub(crate) fn create_paired_pipes(mode: PipeMode) -> io::Result<(File, File)> {
    let mut fds: [c_int; 2] = [0; 2];
    let res = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if res != 0 {
        return Err(io::Error::last_os_error());
    }

    match mode {
        PipeMode::ParentWrites => {
            set_cloexec(fds[1])?;
        }
        PipeMode::ChildWrites => {
            set_cloexec(fds[0])?;
        }
    };

    let reader = unsafe { File::from_raw_fd(fds[0]) };
    let writer = unsafe { File::from_raw_fd(fds[1]) };

    Ok((reader, writer))
}

fn set_cloexec(fd: c_int) -> io::Result<()> {
    let res = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    if res != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn completion_pipes() -> io::Result<(CompletionReceiver, CompletionSender)> {
    let (reader, writer) = create_paired_pipes(PipeMode::ChildWrites)?;

    let reader = CompletionReceiver(reader);
    let writer = CompletionSender(writer);

    Ok((reader, writer))
}

pub(crate) struct CompletionReceiver(File);

impl CompletionReceiver {
    pub(crate) fn recv(&mut self) -> io::Result<()> {
        let mut buf = [0u8; 1];

        if self.0.read(&mut buf)? != 1 {
            Err(io::Error::new(
                io::ErrorKind::Other,
                "child failed to notify parent",
            ))
        } else {
            Ok(())
        }
    }
}

pub(crate) struct CompletionSender(pub(crate) File);

impl CompletionSender {
    pub(crate) fn send(&mut self) -> io::Result<()> {
        if self.0.write(b"1")? != 1 {
            Err(io::Error::new(
                io::ErrorKind::Other,
                "failed to signal parent to close",
            ))
        } else {
            Ok(())
        }
    }
}

pub(crate) trait FdStringExt {
    fn fd_string(&self) -> String;
    unsafe fn from_fd_string(fd_str: &str) -> io::Result<Self>
    where
        Self: Sized;
}

impl<T: AsRawFd + FromRawFd> FdStringExt for T {
    unsafe fn from_fd_string(fd_str: &str) -> io::Result<Self> {
        match fd_str.parse() {
            Ok(fd) => Ok(Self::from_raw_fd(fd)),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid notify socket fd",
            )),
        }
    }

    fn fd_string(&self) -> String {
        self.as_raw_fd().to_string()
    }
}
