//! Concrete listeners over the endpoint spellings a producer already understands.
//!
//! [`PresenterListener`] is a product seam: a product that owns its own transport implements it and
//! hands the presenter accepted connections. But most products want the same two sockets, and three
//! copies of that already existed in this tree before this module — in `vvmux`, in the gateway's
//! smoke test, and in this crate's own test presenter. A binding for another language needs one too,
//! and cannot supply a Rust trait implementation at all.
//!
//! So the ordinary cases live here, addressed the way [`vivid_protocol::wire::Endpoint`] addresses
//! them: `unix:/absolute/path` or `tcp:host:port`.

use std::io;
use std::net::{Shutdown, TcpListener};
#[cfg(unix)]
use std::os::unix::net::UnixListener;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;

use vivid_protocol::wire::Endpoint;

use super::listener::{ConnectionCancel, PresenterListener, Transport};

/// A presenter listener on a Unix socket or a TCP port.
///
/// `tcp:127.0.0.1:0` binds an ephemeral port; [`endpoint`](Self::endpoint) then reports the port the
/// operating system chose, which is what a caller hands to a producer.
#[derive(Debug)]
pub struct SocketListener {
    kind: Bound,
    endpoint: String,
}

#[derive(Debug)]
enum Bound {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix {
        listener: UnixListener,
        path: PathBuf,
    },
}

impl SocketListener {
    /// Bind `unix:/absolute/path` or `tcp:host:port`.
    ///
    /// A Unix path is created with owner-only permissions, and removed when the listener drops. An
    /// existing path is an error rather than something to unlink: a live presenter may be serving
    /// it, and stealing its endpoint is worse than refusing.
    pub fn bind(endpoint: &str) -> io::Result<Self> {
        if let Some(address) = endpoint.strip_prefix("tcp:") {
            // Deliberately not `Endpoint::parse`: it refuses port 0, which is correct for a
            // producer dialling out and wrong for a listener asking the system for an ephemeral
            // port. The loopback restriction it enforces is kept, because a presenter binding a
            // routable interface would expose an authenticated endpoint to the network.
            let parsed = address
                .parse::<std::net::SocketAddrV4>()
                .map_err(|_| invalid("TCP endpoint is not an IPv4 socket address"))?;
            if *parsed.ip() != std::net::Ipv4Addr::LOCALHOST {
                return Err(invalid("TCP endpoint must use exact 127.0.0.1"));
            }
            let listener = TcpListener::bind(parsed)?;
            let resolved = format!("tcp:{}", listener.local_addr()?);
            return Ok(Self {
                kind: Bound::Tcp(listener),
                endpoint: resolved,
            });
        }

        match Endpoint::parse(endpoint)? {
            #[cfg(unix)]
            Endpoint::Unix(path) => {
                let listener = UnixListener::bind(&path)?;
                owner_only(&path)?;
                let resolved = format!("unix:{}", path.display());
                Ok(Self {
                    kind: Bound::Unix { listener, path },
                    endpoint: resolved,
                })
            }
            #[cfg(not(unix))]
            Endpoint::Unix(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unix endpoints are not available on this platform",
            )),
            Endpoint::Tcp(_) => unreachable!("the tcp: prefix is handled above"),
        }
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(unix)]
fn owner_only(path: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

impl Drop for SocketListener {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Bound::Unix { path, .. } = &self.kind {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl PresenterListener for SocketListener {
    fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    fn accept(&self) -> io::Result<Transport> {
        match &self.kind {
            Bound::Tcp(listener) => {
                let (stream, _) = listener.accept()?;
                stream.set_nodelay(true)?;
                let reader = stream.try_clone()?;
                let deadline = stream.try_clone()?;
                let cancel = stream.try_clone()?;
                Ok(Transport::new(
                    Box::new(reader),
                    Box::new(stream),
                    ConnectionCancel::new(move || {
                        let _ = cancel.shutdown(Shutdown::Both);
                    }),
                    Arc::new(move |timeout| deadline.set_read_timeout(timeout)),
                ))
            }
            #[cfg(unix)]
            Bound::Unix { listener, .. } => {
                let (stream, _) = listener.accept()?;
                let reader = stream.try_clone()?;
                let deadline = stream.try_clone()?;
                let cancel = stream.try_clone()?;
                Ok(Transport::new(
                    Box::new(reader),
                    Box::new(stream),
                    ConnectionCancel::new(move || {
                        let _ = cancel.shutdown(Shutdown::Both);
                    }),
                    Arc::new(move |timeout| deadline.set_read_timeout(timeout)),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tcp_listener_reports_the_port_the_system_chose() {
        let listener = SocketListener::bind("tcp:127.0.0.1:0").expect("bind");
        let endpoint = listener.endpoint();

        assert!(
            endpoint.starts_with("tcp:127.0.0.1:"),
            "endpoint: {endpoint}"
        );
        assert!(
            !endpoint.ends_with(":0"),
            "an ephemeral bind reports the resolved port, not the request: {endpoint}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_unix_listener_is_owner_only_and_cleans_up_after_itself() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("presenter.sock");
        let endpoint = format!("unix:{}", path.display());

        {
            let listener = SocketListener::bind(&endpoint).expect("bind");
            assert_eq!(listener.endpoint(), endpoint);

            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the socket is owner-only");

            assert!(
                SocketListener::bind(&endpoint).is_err(),
                "a live endpoint is refused rather than stolen from whoever is serving it"
            );
        }

        assert!(
            !path.exists(),
            "the path is removed when the listener drops"
        );
    }

    #[test]
    fn a_malformed_endpoint_is_refused_before_anything_is_bound() {
        assert!(SocketListener::bind("").is_err());
        assert!(
            SocketListener::bind("127.0.0.1:0").is_err(),
            "the scheme is required"
        );
        assert!(
            SocketListener::bind("unix:relative/path").is_err(),
            "a Unix endpoint must be absolute"
        );
    }
}
