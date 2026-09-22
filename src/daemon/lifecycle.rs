//! Race-safe per-user daemon ownership and Unix-socket request serving.

use crate::{
    ipc::{self, ErrorCode, Operation, PROTOCOL_VERSION, Request, Response, TransportError},
    platform::private_fs::{PrivateDirectory, current_uid},
};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("daemon runtime I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("another daemon or its cleanup guardian owns the runtime lock")]
    AlreadyRunning,
    #[error("unsafe daemon socket ownership, type, or permissions")]
    UnsafeSocket,
    #[error("IPC transport failed: {0}")]
    Transport(#[from] TransportError),
}

/// Owns the permanent lock file and public socket for exactly one daemon.
pub struct DaemonEndpoint {
    _lock: File,
    listener: UnixListener,
    socket_path: PathBuf,
}

impl DaemonEndpoint {
    pub fn bind(runtime: &Path, socket_path: &Path) -> Result<Self, DaemonError> {
        let _runtime = PrivateDirectory::open(runtime)?;
        let lock_path = runtime.join("daemon.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(lock_path)?;
        let metadata = lock.metadata()?;
        if metadata.uid() != current_uid()
            || metadata.mode() & 0o7777 != 0o600
            || !metadata.is_file()
            || metadata.nlink() != 1
        {
            return Err(DaemonError::UnsafeSocket);
        }
        // SAFETY: flock operates on this valid owned descriptor.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Err(DaemonError::AlreadyRunning);
            }
            return Err(error.into());
        }
        match fs::symlink_metadata(socket_path) {
            Ok(metadata) => {
                if metadata.uid() != current_uid() || !metadata.file_type().is_socket() {
                    return Err(DaemonError::UnsafeSocket);
                }
                fs::remove_file(socket_path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(socket_path)?;
        fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            _lock: lock,
            listener,
            socket_path: socket_path.to_owned(),
        })
    }

    pub fn accept(&self) -> io::Result<(UnixStream, std::os::unix::net::SocketAddr)> {
        self.listener.accept()
    }
}

impl Drop for DaemonEndpoint {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
    }
}

pub fn ensure_running(
    socket_path: &Path,
    executable: &Path,
    timeout: Duration,
) -> Result<(), DaemonError> {
    if UnixStream::connect(socket_path).is_ok() {
        return Ok(());
    }
    Command::new(executable)
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if UnixStream::connect(socket_path).is_ok() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(
                io::Error::new(io::ErrorKind::TimedOut, "daemon did not become ready").into(),
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn run(endpoint: DaemonEndpoint, handler: Arc<dyn RequestHandler>) -> Result<(), DaemonError> {
    loop {
        let (stream, _) = endpoint.accept()?;
        let handler = Arc::clone(&handler);
        std::thread::spawn(move || {
            let _ = serve_connection(stream, handler, crate::ipc::DEFAULT_TIMEOUT);
        });
    }
}

pub trait RequestHandler: Send + Sync + 'static {
    fn handle(&self, request: &Request) -> Response;
    fn subscribe(&self) -> Option<std::sync::mpsc::Receiver<crate::ipc::Event>> {
        None
    }
}
impl<F> RequestHandler for F
where
    F: Fn(&Request) -> Response + Send + Sync + 'static,
{
    fn handle(&self, request: &Request) -> Response {
        self(request)
    }
}

pub fn serve_connection(
    mut stream: UnixStream,
    handler: Arc<dyn RequestHandler>,
    timeout: Duration,
) -> Result<(), DaemonError> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let reader_stream = stream.try_clone()?;
    let mut reader = BufReader::new(reader_stream);
    loop {
        let request = match ipc::read_frame::<Request>(&mut reader) {
            Ok(request) => request,
            Err(TransportError::Disconnected) => return Ok(()),
            Err(TransportError::MessageTooLarge) => {
                let response = ipc::failure(
                    uuid::Uuid::nil(),
                    ErrorCode::MessageTooLarge,
                    "message exceeds 1 MiB or is not newline terminated",
                );
                let _ = ipc::write_frame(&mut stream, &response);
                return Ok(());
            }
            Err(TransportError::InvalidMessage(_)) => {
                let response = ipc::failure(
                    uuid::Uuid::nil(),
                    ErrorCode::InvalidRequest,
                    "malformed request",
                );
                let _ = ipc::write_frame(&mut stream, &response);
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let response = if request.protocol_version != PROTOCOL_VERSION {
            ipc::failure(
                request.request_id,
                ErrorCode::UnsupportedVersion,
                format!(
                    "protocol version {} is unsupported; expected {PROTOCOL_VERSION}",
                    request.protocol_version
                ),
            )
        } else {
            handler.handle(&request)
        };
        ipc::write_frame(&mut stream, &response)?;
        if request.operation == Operation::Subscribe {
            if let Some(events) = handler.subscribe() {
                for event in events {
                    if ipc::write_frame(&mut stream, &event).is_err() {
                        break;
                    }
                }
            }
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{os::unix::net::UnixStream, thread};
    fn private_tempdir() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        root
    }
    #[test]
    fn concurrent_bind_has_one_owner_and_stale_socket_is_cleaned() {
        let root = private_tempdir();
        let socket = root.path().join("tdeck.sock");
        let first = DaemonEndpoint::bind(root.path(), &socket).unwrap();
        assert!(matches!(
            DaemonEndpoint::bind(root.path(), &socket),
            Err(DaemonError::AlreadyRunning)
        ));
        drop(first);
        UnixListener::bind(&socket).unwrap();
        let endpoint = DaemonEndpoint::bind(root.path(), &socket).unwrap();
        assert_eq!(
            fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(endpoint);
    }

    #[test]
    fn simultaneous_start_attempts_create_exactly_one_daemon() {
        use std::sync::{Arc, Barrier};
        let root = private_tempdir();
        let runtime = root.path().to_path_buf();
        let start = Arc::new(Barrier::new(8));
        let finish = Arc::new(Barrier::new(8));
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let runtime = runtime.clone();
                let start = Arc::clone(&start);
                let finish = Arc::clone(&finish);
                thread::spawn(move || {
                    start.wait();
                    let endpoint = DaemonEndpoint::bind(&runtime, &runtime.join("tdeck.sock"));
                    finish.wait();
                    endpoint
                })
            })
            .collect();
        let results: Vec<_> = tasks.into_iter().map(|task| task.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(DaemonError::AlreadyRunning)))
                .count(),
            7
        );
    }
    #[test]
    fn unsafe_socket_is_not_removed() {
        let root = private_tempdir();
        let socket = root.path().join("tdeck.sock");
        fs::write(&socket, "keep").unwrap();
        assert!(matches!(
            DaemonEndpoint::bind(root.path(), &socket),
            Err(DaemonError::UnsafeSocket)
        ));
        assert_eq!(fs::read_to_string(socket).unwrap(), "keep");
    }

    #[test]
    fn unsafe_runtime_permissions_are_rejected_without_repair() {
        let root = private_tempdir();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(DaemonEndpoint::bind(root.path(), &root.path().join("tdeck.sock")).is_err());
        assert_eq!(
            fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn malformed_and_oversized_frames_receive_structured_errors() {
        for (bytes, expected) in [
            (b"not-json\n".to_vec(), ErrorCode::InvalidRequest),
            (
                vec![b'x'; crate::ipc::MAX_MESSAGE_BYTES + 2],
                ErrorCode::MessageTooLarge,
            ),
        ] {
            let (server, mut client) = UnixStream::pair().unwrap();
            let task = thread::spawn(move || {
                serve_connection(
                    server,
                    Arc::new(|request: &Request| ipc::success(request.request_id, json!({}))),
                    Duration::from_secs(1),
                )
                .unwrap()
            });
            use std::io::Write;
            client.write_all(&bytes).unwrap();
            let response: Response = ipc::read_frame(&mut BufReader::new(&client)).unwrap();
            assert!(matches!(response, Response::Failure(value) if value.error.code == expected));
            drop(client);
            task.join().unwrap();
        }
    }
    #[test]
    fn mismatch_is_structured_and_disconnect_is_clean() {
        let (server, mut client) = UnixStream::pair().unwrap();
        let task = thread::spawn(move || {
            serve_connection(
                server,
                Arc::new(|request: &Request| ipc::success(request.request_id, json!({}))),
                Duration::from_secs(1),
            )
            .unwrap()
        });
        let request = Request {
            protocol_version: 2,
            request_id: uuid::Uuid::new_v4(),
            operation: Operation::Status,
            payload: json!({}),
        };
        ipc::write_frame(&mut client, &request).unwrap();
        let response: Response = ipc::read_frame(&mut BufReader::new(&client)).unwrap();
        assert!(
            matches!(response, Response::Failure(value) if value.error.code == ErrorCode::UnsupportedVersion)
        );
        drop(client);
        task.join().unwrap();
    }

    #[test]
    fn client_and_daemon_exchange_correlated_requests_in_temp_runtime() {
        let root = private_tempdir();
        let socket = root.path().join("tdeck.sock");
        let endpoint = DaemonEndpoint::bind(root.path(), &socket).unwrap();
        let task = thread::spawn(move || {
            let (stream, _) = endpoint.accept().unwrap();
            serve_connection(
                stream,
                Arc::new(|request: &Request| {
                    ipc::success(request.request_id, json!({"healthy":true}))
                }),
                Duration::from_secs(1),
            )
            .unwrap();
        });
        let request = Request::new(Operation::Status, json!({}));
        let response = crate::ipc::Client::connect(&socket, Duration::from_secs(1))
            .unwrap()
            .call(&request)
            .unwrap();
        assert!(
            matches!(response, Response::Success(value) if value.request_id == request.request_id && value.result["healthy"] == true)
        );
        task.join().unwrap();
    }
}
