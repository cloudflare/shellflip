use command_group::{CommandGroup, Signal, UnixChildExt};
use sendfd::RecvWithFd;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixDatagram;
use std::process::{Command, Stdio};
use test_binary::build_test_binary_once;

build_test_binary_once!(basic, "testbins");

#[track_caller]
fn assert_line<R: Read>(stdout: &mut BufReader<R>, expected: &str) {
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert_eq!(line, expected);
}

#[test]
fn test_restart_on_signal() {
    let mut test_process = Command::new(path_to_basic())
        .stdout(Stdio::piped())
        .group_spawn()
        .expect("error running test binary");

    let mut stdout = BufReader::new(test_process.inner().stdout.take().unwrap());
    assert_line(&mut stdout, "Started with generation 0\n");

    // Try reloading the process every time.
    for i in 1..6 {
        test_process.signal(Signal::SIGUSR1).unwrap();
        // This is output from the child process
        assert_line(&mut stdout, &format!("Started with generation {}\n", i));
        // This is output from the parent process
        assert_line(&mut stdout, "Restart successful\n");

        // The parent terminates immediately after the child starts successfully
        assert!(test_process
            .wait()
            .expect("error waiting for test binary")
            .success());
    }

    // The 6th restart fails; we should see this on stdout
    test_process.signal(Signal::SIGUSR1).unwrap();
    // This is output from the child process
    assert_line(
        &mut stdout,
        "Restart task failed: Restart failed: The operation completed successfully\n",
    );

    test_process.kill().unwrap();
}

#[test]
fn test_restart_coordination_socket() {
    let sock_args = ["--socket", "/tmp/restarter.sock"];

    let mut test_process = Command::new(path_to_basic())
        .args(sock_args)
        .stdout(Stdio::piped())
        .group_spawn()
        .expect("error running test binary");

    let mut stdout = BufReader::new(test_process.inner().stdout.take().unwrap());
    assert_line(&mut stdout, "Started with generation 0\n");

    for i in 1..6 {
        let test_restart_output = Command::new(path_to_basic())
            .args(sock_args)
            .arg("restart")
            .output()
            .expect("error running test binary");

        assert!(test_restart_output.status.success());
        assert_eq!(
            std::str::from_utf8(&test_restart_output.stdout).unwrap(),
            "Restart request succeeded\n"
        );

        // This is output from the child process
        assert_line(&mut stdout, &format!("Started with generation {}\n", i));
        // This is output from the parent process
        assert_line(&mut stdout, "Restart successful\n");

        // The parent terminates immediately after the child starts successfully
        assert!(test_process
            .wait()
            .expect("error waiting for test binary")
            .success());
    }

    // The 6th restart fails; this should be propagated to the restart invoker
    let test_restart_output = Command::new(path_to_basic())
        .args(sock_args)
        .arg("restart")
        .output()
        .expect("error running test binary");

    assert!(!test_restart_output.status.success());
    assert_eq!(
        std::str::from_utf8(&test_restart_output.stdout).unwrap(),
        "Restart request failed: Child failed to start: The operation completed successfully\n"
    );

    test_process.kill().unwrap();
}

#[test]
fn test_systemd() {
    let notify_socket_path = "/tmp/restart_notify.sock";
    let _ = std::fs::remove_file(notify_socket_path);
    let notify_socket = UnixDatagram::bind(notify_socket_path).unwrap();
    notify_socket
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .unwrap();

    let mut test_process = Command::new(path_to_basic())
        .stdout(Stdio::piped())
        .env("NOTIFY_SOCKET", notify_socket_path)
        .arg("--systemd")
        .group_spawn()
        .expect("error running test binary");

    let mut stdout = BufReader::new(test_process.inner().stdout.take().unwrap());
    assert_line(&mut stdout, "Started with generation 0\n");

    let recv_notify_messages = || {
        let mut buf = [0u8; 1024];
        let mut fds = [0i32; 32];
        let (n, fd_count) = notify_socket.recv_with_fd(&mut buf, &mut fds).unwrap();
        let messages = std::str::from_utf8(&buf[..n]).unwrap();
        (
            messages.split('\n').map(String::from).collect::<Vec<_>>(),
            fds[..fd_count].to_vec(),
        )
    };

    let (message_fdstore, stored_fds) = recv_notify_messages();
    assert_eq!(message_fdstore[0], "FDSTORE=1");
    assert_eq!(message_fdstore[1], "FDNAME=sf_mem_handover");
    assert_eq!(message_fdstore[2], "");
    assert_eq!(stored_fds.len(), 1);
    assert_eq!(recv_notify_messages().0[0], "READY=1");

    let mut memfd = unsafe { File::from_raw_fd(stored_fds[0]) };
    assert_eq!(memfd.metadata().unwrap().len(), 0);

    // Trigger restart in the process (i.e. a shutdown) and see what it wrote to the memfd
    test_process.signal(Signal::SIGUSR1).unwrap();
    assert!(test_process
        .wait()
        .expect("error waiting for test binary")
        .success());

    let mut serialized = vec![0u8; 0];
    memfd.rewind().unwrap();
    memfd.read_to_end(&mut serialized).unwrap();
    // We always write a magic number to the beginning of the data
    assert_eq!(
        u32::from_be_bytes(serialized[0..4].try_into().unwrap()),
        0xCAFEF00D
    );
}
