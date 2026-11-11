//! Nonblocking scoped clipboard requests retain exact authority across cancellation.
#![cfg(feature = "gui")]

// Rust guideline compliant 2026-10-07
use std::{io, sync::mpsc, thread, time::Duration};
use vivid_sdk::gui_host::clipboard::{Host, Target};

#[test]
fn files_are_explicit_bounded_metadata_and_text_only_services_reject_them() {
    use vivid_sdk::gui_host::clipboard::{FileList, Format};
    let path = std::env::temp_dir().join("clipboard #λ filename");
    let files = FileList::from_paths(std::slice::from_ref(&path)).unwrap();
    assert_eq!(files.paths().unwrap(), vec![path]);
    assert!(!format!("{files:?}").contains("filename"));
    assert!(FileList::from_uri_list("file://remote/private").is_err());
    let (text_host, text) = Host::new(false);
    text_host.set_active(true);
    assert!(!text.has_file_lists());
    assert_eq!(
        text.write_files(files.clone()).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert!(text.begin_read_files().is_err());
    let (host, client) = Host::with_file_lists(true);
    host.set_active(true);
    client.write_files(files.clone()).unwrap();
    let request = host.next_request().unwrap();
    assert_eq!(
        (request.target(), request.format()),
        (Target::Clipboard, Format::Files)
    );
    assert!(request.write_text().is_none());
    assert_eq!(request.write_files(), Some(&files));
    request.complete(Ok(String::new()));
    let mut read = client.begin_read_files().unwrap();
    let request = host.next_request().unwrap();
    request.complete(Ok(format!(
        "{}https://example.org/secret\r\n",
        files.as_uri_list()
    )));
    assert_eq!(
        read.poll().unwrap().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn file_and_text_jobs_share_capacity_and_late_file_replies_cannot_restore_revoked_scopes() {
    use vivid_sdk::gui_host::clipboard::{FileList, Format};
    let (host, client) = Host::with_file_lists(true);
    host.set_active(true);
    let (scope, a) = client.scoped().unwrap();
    let (_peer_scope, peer) = client.scoped().unwrap();
    let mut files = a.begin_read_files().unwrap();
    let mut text = peer.begin_read(Target::Primary).unwrap();
    let extra_file = peer.begin_read_files().unwrap();
    let extra_text = peer.begin_read(Target::Clipboard).unwrap();
    assert_eq!(
        client.begin_read_files().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let stale = host.next_request().unwrap();
    scope.revoke();
    let data = FileList::from_paths(&[std::env::temp_dir().join("private")]).unwrap();
    assert!(
        stale
            .lease()
            .while_live(|| panic!("revoked file write ran"))
            .is_none()
    );
    stale.complete(Ok(data.into_uri_list()));
    assert_eq!(
        files.poll().unwrap().unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    let live = host.next_request().unwrap();
    assert_eq!(live.format(), Format::Text);
    live.complete(Ok("peer selection survives".into()));
    assert_eq!(text.poll().unwrap().unwrap(), "peer selection survives");
    drop(extra_file);
    drop(extra_text);
    let mut fresh = peer.begin_read_files().unwrap();
    let data = FileList::from_paths(&[std::env::temp_dir().join("peer")]).unwrap();
    host.next_request()
        .unwrap()
        .complete(Ok(data.as_uri_list().into()));
    assert_eq!(fresh.poll().unwrap().unwrap(), data);
    assert!(fresh.poll().is_none());
}

#[cfg(feature = "gui-testing")]
#[test]
fn platform_file_reads_require_file_entries_and_preserve_exact_native_paths() {
    use vivid_sdk::gui::{
        ClipboardEntry, ClipboardItem, ExternalPaths, TestAppContext, TestDispatcher,
    };
    use vivid_sdk::gui_host::clipboard::FileList;
    let cx = TestAppContext::build(TestDispatcher::new(0), None);
    let path = std::env::temp_dir().join("clipboard λ # ?.txt");
    cx.update(|app| {
        app.write_to_clipboard(ClipboardItem {
            entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(
                [path.clone()].into_iter().collect(),
            ))],
        })
    });
    let read = cx.read(|app| app.read_clipboard_uri_list_async(1024 * 1024));
    let data = cx.foreground_executor().block_test(read).unwrap().unwrap();
    assert_eq!(
        FileList::from_uri_list(&data).unwrap().paths().unwrap(),
        vec![path]
    );
    cx.update(|app| app.write_to_clipboard(ClipboardItem::new_string(data)));
    let read = cx.read(|app| app.read_clipboard_uri_list_async(1024 * 1024));
    assert_eq!(cx.foreground_executor().block_test(read).unwrap(), None);
}

#[cfg(feature = "gui-testing")]
#[test]
fn platform_text_reads_preserve_unicode_and_reject_excess_before_concatenation() {
    use vivid_sdk::gui::{
        ClipboardItem, ClipboardReadError, ClipboardSelection, TestAppContext, TestDispatcher,
    };
    let cx = TestAppContext::build(TestDispatcher::new(0), None);
    let text = "λ\r\n👋";
    cx.update(|app| app.write_to_clipboard(ClipboardItem::new_string(text.into())));
    let read =
        cx.read(|app| app.read_clipboard_text_async(ClipboardSelection::Clipboard, text.len()));
    assert_eq!(
        cx.foreground_executor()
            .block_test(read)
            .unwrap()
            .as_deref(),
        Some(text)
    );
    let read =
        cx.read(|app| app.read_clipboard_text_async(ClipboardSelection::Clipboard, text.len() - 1));
    assert_eq!(
        cx.foreground_executor().block_test(read).unwrap_err(),
        ClipboardReadError::UnsupportedContent
    );
}

#[test]
fn sibling_scopes_cancel_exact_requests_without_retiring_the_gui_owner() {
    let (host, client) = Host::new(true);
    host.set_active(true);
    let (scope_a, a) = client.scoped().unwrap();
    let (_scope_b, b) = client.scoped().unwrap();
    let mut pending_a = a.begin_read(Target::Clipboard).unwrap();
    let mut pending_b = b.begin_read(Target::Primary).unwrap();
    assert!(pending_a.poll().is_none());
    let stale = host.next_request().unwrap();
    drop(scope_a);
    assert!(!stale.is_live());
    assert_eq!(
        pending_a.poll().unwrap().unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(
        stale
            .lease()
            .while_live(|| panic!("stale platform work ran"))
            .is_none()
    );
    stale.complete(Ok("revoked bytes".into()));
    host.next_request()
        .unwrap()
        .complete(Ok("live sibling bytes".into()));
    assert_eq!(pending_b.poll().unwrap().unwrap(), "live sibling bytes");
    assert!(pending_b.poll().is_none());
    client
        .write(Target::Clipboard, "GUI owner survives".into())
        .unwrap();
    assert_eq!(
        host.next_request().unwrap().write_text(),
        Some("GUI owner survives")
    );
}

#[test]
fn scope_revocation_serializes_with_bounded_platform_access() {
    let (host, client) = Host::new(false);
    host.set_active(true);
    let (scope, client) = client.scoped().unwrap();
    client
        .write(Target::Clipboard, "private contents".into())
        .unwrap();
    let request = host.next_request().unwrap();
    let lease = request.lease();
    let (entered, started) = mpsc::sync_channel(1);
    let (release, finish) = mpsc::sync_channel(1);
    let operation = thread::spawn(move || {
        lease.while_live(|| {
            entered.send(()).unwrap();
            finish.recv_timeout(Duration::from_secs(2)).unwrap();
        })
    });
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let (done, revoked) = mpsc::sync_channel(1);
    let revocation = thread::spawn(move || {
        scope.revoke();
        done.send(()).unwrap();
    });
    assert!(matches!(
        revoked.recv_timeout(Duration::from_millis(25)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release.send(()).unwrap();
    assert_eq!(operation.join().unwrap(), Some(()));
    revoked.recv_timeout(Duration::from_secs(2)).unwrap();
    revocation.join().unwrap();
    assert!(!request.is_live());
    assert_eq!(
        client
            .write(Target::Clipboard, "late contents".into())
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
}

#[test]
fn cancellation_reclaims_queued_capacity_and_scope_depth_is_bounded() {
    let (host, client) = Host::new(false);
    host.set_active(true);
    let (scope, scoped) = client.scoped().unwrap();
    let pending: Vec<_> = (0..4)
        .map(|_| scoped.begin_read(Target::Clipboard).unwrap())
        .collect();
    drop(scope);
    client
        .write(Target::Clipboard, "new authority".into())
        .unwrap();
    assert_eq!(
        host.next_request().unwrap().write_text(),
        Some("new authority")
    );
    drop(pending);
    let mut current = client;
    let mut guards = Vec::new();
    for _ in 0..8 {
        let (scope, next) = current.scoped().unwrap();
        guards.push(scope);
        current = next;
    }
    assert!(current.scoped().is_err());
    guards[0].revoke();
    assert!(current.begin_read(Target::Clipboard).is_err());
}
