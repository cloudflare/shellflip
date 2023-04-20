use libc::c_int;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd};

/// Create a pair of pipes. The read end (first element) is not inheritable
/// in child processes but the write end (second element) is.
pub(crate) fn pipe_pair() -> io::Result<(CompletionReceiver, CompletionSender)> {
    let mut fds: [c_int; 2] = [0; 2];
    let res = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if res != 0 {
        return Err(io::Error::last_os_error());
    }
    set_cloexec(fds[0])?;

    let reader = CompletionReceiver(unsafe { File::from_raw_fd(fds[0]) });
    let writer = CompletionSender(unsafe { File::from_raw_fd(fds[1]) });

    Ok((reader, writer))
}

fn set_cloexec(fd: c_int) -> io::Result<()> {
    let res = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    if res != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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

pub(crate) struct CompletionSender(File);

impl CompletionSender {
    pub(crate) fn from_fd_string(fd_str: &str) -> io::Result<Self> {
        match fd_str.parse() {
            Ok(fd) => Ok(CompletionSender(unsafe { File::from_raw_fd(fd) })),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid notify socket fd",
            )),
        }
    }

    pub(crate) fn fd_string(&self) -> String {
        self.0.as_raw_fd().to_string()
    }

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
