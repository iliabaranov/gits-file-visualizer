//! Unix-socket server. Each client gets a writer thread fed through a bounded
//! channel; a client that falls too far behind is dropped (it reconnects and
//! receives a fresh snapshot).

use gits_core::protocol;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, channel, sync_channel};
use std::sync::{Arc, Mutex};

const CLIENT_QUEUE: usize = 512;

struct Client {
    id: u64,
    tx: SyncSender<Arc<Vec<u8>>>,
}

pub struct Server {
    path: PathBuf,
    incoming: Receiver<UnixStream>,
    clients: Vec<Client>,
    next_id: u64,
    status: Arc<Mutex<String>>,
}

impl Server {
    pub fn start(path: &Path) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        if path.exists() {
            if UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, format!("another collector is already serving {}", path.display())));
            }
            std::fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let status = Arc::new(Mutex::new(String::from("starting")));
        let (tx, incoming) = channel();
        let st = status.clone();
        std::thread::Builder::new().name("accept".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut s = &stream;
                let text = st.lock().map(|g| g.clone()).unwrap_or_default();
                if s.write_all(&protocol::hello_frame()).and_then(|_| s.write_all(&protocol::status_frame(&text))).is_err() {
                    continue;
                }
                if tx.send(stream).is_err() {
                    break;
                }
            }
        })?;
        Ok(Self { path: path.to_path_buf(), incoming, clients: vec![], next_id: 1, status })
    }

    /// Status line sent to clients as they connect (e.g. while scanning).
    pub fn set_status(&self, s: impl Into<String>) {
        if let Ok(mut g) = self.status.lock() {
            *g = s.into();
        }
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// Registers clients that connected since the last call and sends each a
    /// snapshot. Call only at a batch boundary (no unsent operations).
    pub fn accept_new(&mut self, snapshot: impl FnOnce() -> Vec<u8>) {
        let mut snapshot = Some(snapshot);
        let mut frame: Option<Arc<Vec<u8>>> = None;
        loop {
            let stream = match self.incoming.try_recv() {
                Ok(s) => s,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            };
            let frame = frame.get_or_insert_with(|| Arc::new((snapshot.take().unwrap())())).clone();
            let (tx, rx) = sync_channel::<Arc<Vec<u8>>>(CLIENT_QUEUE);
            let id = self.next_id;
            self.next_id += 1;
            let spawned = std::thread::Builder::new().name(format!("client-{id}")).spawn(move || {
                let mut s = stream;
                while let Ok(buf) = rx.recv() {
                    if s.write_all(&buf).is_err() {
                        break;
                    }
                }
            });
            if spawned.is_ok() && tx.try_send(frame).is_ok() {
                log::info!("client {id} connected ({} total)", self.clients.len() + 1);
                self.clients.push(Client { id, tx });
            }
        }
    }

    pub fn broadcast(&mut self, frame: Vec<u8>) {
        if self.clients.is_empty() {
            return;
        }
        let frame = Arc::new(frame);
        self.clients.retain(|c| match c.tx.try_send(frame.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                log::warn!("client {} is too slow; dropping it", c.id);
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                log::info!("client {} disconnected", c.id);
                false
            }
        });
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
