//! Where the filesystem model comes from: the synthetic work session, or the
//! gits-collector service over its Unix socket.

use gits_core::model::Model;
use gits_core::protocol::{self, Msg, Op};
use gits_core::synth::{Index, Session};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

pub enum Source {
    Synthetic { index: Index, session: Session },
    Collector(CollectorClient),
}

pub enum Incoming {
    Status(String),
    /// Decoded on the client thread; the epoch is fixed up by the receiver.
    Snapshot(Box<Model>),
    Ops(Vec<Op>),
    Disconnected(String),
}

pub struct CollectorClient {
    rx: Receiver<Incoming>,
    pub has_snapshot: bool,
}

/// True if a collector is accepting connections at `socket`.
pub fn collector_available(socket: &Path) -> bool {
    UnixStream::connect(socket).is_ok()
}

impl CollectorClient {
    pub fn spawn(socket: PathBuf) -> Self {
        let (tx, rx) = channel();
        let path = socket;
        std::thread::Builder::new()
            .name("collector-client".into())
            .spawn(move || client_thread(path, tx))
            .expect("spawning collector client thread");
        Self { rx, has_snapshot: false }
    }

    pub fn try_recv(&self) -> Option<Incoming> {
        self.rx.try_recv().ok()
    }
}

fn client_thread(path: PathBuf, tx: Sender<Incoming>) {
    let mut announced = false;
    loop {
        match UnixStream::connect(&path) {
            Ok(stream) => {
                announced = false;
                let err = serve(stream, &tx);
                if tx.send(Incoming::Disconnected(err)).is_err() {
                    return;
                }
            }
            Err(e) => {
                if !announced {
                    announced = true;
                    if tx.send(Incoming::Disconnected(format!("cannot connect to {}: {e}", path.display()))).is_err() {
                        return;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn serve(stream: UnixStream, tx: &Sender<Incoming>) -> String {
    let mut r = std::io::BufReader::with_capacity(1 << 20, stream);
    loop {
        let msg = match protocol::read_msg(&mut r) {
            Ok(m) => m,
            Err(e) => return format!("collector connection lost: {e}"),
        };
        let item = match msg {
            Msg::Hello { version } if version != protocol::VERSION => {
                return format!("collector speaks protocol v{version}, expected v{}", protocol::VERSION);
            }
            Msg::Hello { .. } => continue,
            Msg::Status(s) => Incoming::Status(s),
            Msg::Snapshot(bytes) => match protocol::decode_snapshot(&bytes, 0) {
                Ok(m) => Incoming::Snapshot(Box::new(m)),
                Err(e) => return format!("bad snapshot: {e}"),
            },
            Msg::Ops(ops) => Incoming::Ops(ops),
        };
        if tx.send(item).is_err() {
            return "renderer stopped".into();
        }
    }
}
