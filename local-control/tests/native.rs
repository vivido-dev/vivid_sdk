//! Native ownership, deadlines and cancellation over real local transports.

// Rust guideline compliant 2026-10-07
use std::{
    io::{self, Read, Write},
    path::PathBuf,
    time::Duration,
};
use vivid_local_control::{LocalListener, LocalStream};

fn endpoint(dir: &tempfile::TempDir) -> PathBuf {
    #[cfg(unix)]
    {
        dir.path().join("control.sock")
    }
    #[cfg(windows)]
    {
        PathBuf::from(format!(
            r"\\.\pipe\vivid-local-{}-{}",
            std::process::id(),
            dir.path().file_name().unwrap().to_string_lossy()
        ))
    }
}

#[test]
fn timed_accept_can_be_reused_and_never_replaces_an_existing_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = endpoint(&dir);
    let listener = LocalListener::bind(&path).unwrap();
    assert_eq!(
        listener
            .accept_timeout(Duration::from_millis(20))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert!(LocalListener::bind(&path).is_err());
    let dial = std::thread::spawn(move || LocalStream::connect(&path).unwrap());
    let mut accepted = listener.accept_timeout(Duration::from_secs(3)).unwrap();
    let mut client = dial.join().unwrap();
    client.write_all(b"owned").unwrap();
    let mut bytes = [0; 5];
    accepted.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"owned");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(endpoint(&dir))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn read_timeout_retires_its_operation_and_shutdown_wakes_a_clone() {
    let (mut left, mut right) = LocalStream::pair().unwrap();
    left.set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let error = left.read(&mut [0]).unwrap_err();
    assert!(matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ));
    right.write_all(b"x").unwrap();
    left.read_exact(&mut [0]).unwrap();
    left.set_read_timeout(None).unwrap();
    let cancel = left.try_clone().unwrap();
    let read = std::thread::spawn(move || left.read(&mut [0]));
    cancel.shutdown().unwrap();
    assert_eq!(read.join().unwrap().unwrap(), 0);
}
