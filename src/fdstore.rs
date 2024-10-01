use memfile::MemFile;
use std::io;
use std::pin::Pin;
use tokio::fs::File;

const SHELLFLIP_FD_PREFIX: &str = "sf_";
const SYSTEMD_MEMFD_NAME: &str = "mem_handover";

pub(crate) fn create_handover_memfd() -> io::Result<Pin<Box<File>>> {
    let memfd = MemFile::create_default("shellflip_restart")?;

    sd_notify::notify_with_fds(
        false,
        &[
            sd_notify::NotifyState::FdStore,
            sd_notify::NotifyState::FdName(&format!("{SHELLFLIP_FD_PREFIX}{SYSTEMD_MEMFD_NAME}")),
        ],
        &[memfd.as_fd()],
    )?;

    Ok(Box::pin(File::from_std(memfd.into_file())))
}
