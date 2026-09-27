//! Collector → renderer protocol over a Unix socket.
//!
//! Frames are `[u32 payload length][u8 kind][payload]`, little-endian. A client
//! receives `Hello`, optionally `Status` lines while the collector scans, one
//! `Snapshot` of the whole model, then `Ops` batches. Both sides apply the same
//! operations in the same order to their own [`Model`], so file and directory
//! ids stay identical without ever being sent explicitly on creation.

use crate::model::{Dir, DirId, File, FileId, FileKind, Model, NEVER, child_key, hash_str};
use std::io::{self, Read, Write};
use std::path::PathBuf;

pub const MAGIC: u32 = 0x5354_4947; // "GITS"
pub const VERSION: u16 = 1;
const MAX_FRAME: usize = 1 << 30;

const K_HELLO: u8 = 0;
const K_SNAPSHOT: u8 = 1;
const K_OPS: u8 = 2;
const K_STATUS: u8 = 3;

/// One change to the model. Applying it assigns ids exactly like the collector did.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    MkDir { parent: DirId, name: String },
    RmDir { dir: DirId },
    Create { dir: DirId, name: String, size: u64, mtime: i64, symlink: bool },
    Delete { file: FileId },
    Read { file: FileId },
    Write { file: FileId, size: u64, mtime: i64 },
}

#[derive(Debug)]
pub enum Msg {
    Hello {
        version: u16,
    },
    /// Encoded model (decode with [`decode_snapshot`]; kept raw so it can be
    /// decoded off the render thread).
    Snapshot(Vec<u8>),
    Ops(Vec<Op>),
    Status(String),
}

/// `$XDG_RUNTIME_DIR/gits-file-visualizer/collector.sock`.
pub fn default_socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| {
        let uid = std::env::var("UID").unwrap_or_else(|_| "user".into());
        std::env::temp_dir().join(format!("gits-file-visualizer-{uid}"))
    });
    base.join("gits-file-visualizer").join("collector.sock")
}

impl Model {
    /// Applies an operation at model time `t`. Returns false (and changes
    /// nothing) if it refers to unknown ids, e.g. before the first snapshot.
    pub fn apply(&mut self, op: &Op, t: f64) -> bool {
        let nd = self.dirs.len() as u32;
        let nf = self.files.len() as u32;
        match op {
            Op::MkDir { parent, name } => {
                if *parent >= nd {
                    return false;
                }
                self.add_dir(*parent, name.clone());
            }
            Op::RmDir { dir } => {
                if *dir >= nd {
                    return false;
                }
                self.remove_dir(*dir, t);
            }
            Op::Create { dir, name, size, mtime, symlink } => {
                if *dir >= nd {
                    return false;
                }
                let f = self.create(*dir, name.clone(), *size, t);
                let file = &mut self.files[f as usize];
                file.mtime = *mtime;
                file.symlink = *symlink;
            }
            Op::Delete { file } => {
                if *file >= nf {
                    return false;
                }
                self.delete(*file, t);
            }
            Op::Read { file } => {
                if *file >= nf {
                    return false;
                }
                self.read(*file, t);
            }
            Op::Write { file, size, mtime } => {
                if *file >= nf {
                    return false;
                }
                self.write(*file, t, Some(*size));
                self.files[*file as usize].mtime = *mtime;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

struct Buf(Vec<u8>);

impl Buf {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
}

struct Cur<'a> {
    b: &'a [u8],
    pos: usize,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(|| bad("truncated frame"))?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> io::Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn str(&mut self) -> io::Result<String> {
        let n = self.u32()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn ids(&mut self) -> io::Result<Vec<u32>> {
        let n = self.u32()? as usize;
        if n > self.b.len() / 4 {
            return Err(bad("id list too long"));
        }
        (0..n).map(|_| self.u32()).collect()
    }
}

fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 5);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    out
}

pub fn hello_frame() -> Vec<u8> {
    let mut b = Buf(vec![]);
    b.u32(MAGIC);
    b.u16(VERSION);
    frame(K_HELLO, &b.0)
}

pub fn status_frame(s: &str) -> Vec<u8> {
    let mut b = Buf(vec![]);
    b.str(s);
    frame(K_STATUS, &b.0)
}

pub fn snapshot_frame(m: &Model) -> Vec<u8> {
    frame(K_SNAPSHOT, &encode_snapshot(m))
}

pub fn ops_frame(ops: &[Op]) -> Vec<u8> {
    let mut b = Buf(Vec::with_capacity(ops.len() * 16));
    b.u32(ops.len() as u32);
    for op in ops {
        match op {
            Op::MkDir { parent, name } => {
                b.u8(0);
                b.u32(*parent);
                b.str(name);
            }
            Op::RmDir { dir } => {
                b.u8(1);
                b.u32(*dir);
            }
            Op::Create { dir, name, size, mtime, symlink } => {
                b.u8(2);
                b.u32(*dir);
                b.str(name);
                b.u64(*size);
                b.i64(*mtime);
                b.u8(*symlink as u8);
            }
            Op::Delete { file } => {
                b.u8(3);
                b.u32(*file);
            }
            Op::Read { file } => {
                b.u8(4);
                b.u32(*file);
            }
            Op::Write { file, size, mtime } => {
                b.u8(5);
                b.u32(*file);
                b.u64(*size);
                b.i64(*mtime);
            }
        }
    }
    frame(K_OPS, &b.0)
}

pub fn encode_snapshot(m: &Model) -> Vec<u8> {
    let mut b = Buf(Vec::with_capacity(m.files.len() * 40 + m.dirs.len() * 40));
    b.str(&m.dirs[m.root as usize].name);
    b.u32(m.dirs.len() as u32);
    for d in &m.dirs {
        b.u32(d.parent.unwrap_or(u32::MAX));
        b.str(&d.name);
        b.u8((d.removed as u8) | ((d.purged as u8) << 1));
    }
    b.u32(m.files.len() as u32);
    for f in &m.files {
        b.u32(f.dir);
        b.str(&f.name);
        b.u64(f.size);
        b.i64(f.mtime);
        let gone = f.purged || f.t_delete != NEVER;
        b.u8((f.symlink as u8) | ((gone as u8) << 1));
    }
    for d in &m.dirs {
        b.u32(d.dirs.len() as u32);
        d.dirs.iter().for_each(|&c| b.u32(c));
        b.u32(d.files.len() as u32);
        d.files.iter().for_each(|&f| b.u32(f));
    }
    b.0
}

/// Decodes a snapshot. Files already deleted on the collector side arrive
/// purged (no ghosts for things removed before this client connected).
pub fn decode_snapshot(bytes: &[u8], epoch_unix: i64) -> io::Result<Model> {
    let mut c = Cur { b: bytes, pos: 0 };
    let root_name = c.str()?;
    let nd = c.u32()? as usize;
    if nd == 0 || nd > bytes.len() / 9 {
        return Err(bad("bad directory count"));
    }
    let mut dirs: Vec<Dir> = Vec::with_capacity(nd);
    for i in 0..nd {
        let parent = c.u32()?;
        let name = c.str()?;
        let flags = c.u8()?;
        let (parent, depth, key) = if i == 0 {
            (None, 0, hash_str(&root_name))
        } else {
            let p = dirs.get(parent as usize).ok_or_else(|| bad("directory parent out of order"))?;
            (Some(parent), p.depth + 1, child_key(p.key, &name))
        };
        dirs.push(Dir { name, parent, depth, dirs: vec![], files: vec![], removed: flags & 1 != 0, purged: flags & 2 != 0, key });
    }
    let nf = c.u32()? as usize;
    if nf > bytes.len() / 25 {
        return Err(bad("bad file count"));
    }
    let mut files: Vec<File> = Vec::with_capacity(nf);
    for _ in 0..nf {
        let dir = c.u32()?;
        if dir as usize >= nd {
            return Err(bad("file directory out of range"));
        }
        let name = c.str()?;
        let size = c.u64()?;
        let mtime = c.i64()?;
        let flags = c.u8()?;
        let gone = flags & 2 != 0;
        files.push(File {
            kind: FileKind::from_name(&name),
            hidden: name.starts_with('.'),
            symlink: flags & 1 != 0,
            name,
            dir,
            size,
            mtime,
            t_read: NEVER,
            t_write: NEVER,
            t_create: NEVER,
            t_delete: if gone { 0.0 } else { NEVER },
            purged: gone,
            arc: 0.0,
        });
    }
    for d in dirs.iter_mut() {
        d.dirs = c.ids()?;
        d.files = c.ids()?;
        if d.dirs.iter().any(|&x| x as usize >= nd) || d.files.iter().any(|&x| x as usize >= nf) {
            return Err(bad("child id out of range"));
        }
        d.files.retain(|&f| !files[f as usize].purged);
    }
    Ok(Model::from_parts(dirs, files, epoch_unix))
}

fn decode_ops(payload: &[u8]) -> io::Result<Vec<Op>> {
    let mut c = Cur { b: payload, pos: 0 };
    let n = c.u32()? as usize;
    if n > payload.len() {
        return Err(bad("bad op count"));
    }
    let mut ops = Vec::with_capacity(n);
    for _ in 0..n {
        ops.push(match c.u8()? {
            0 => Op::MkDir { parent: c.u32()?, name: c.str()? },
            1 => Op::RmDir { dir: c.u32()? },
            2 => Op::Create { dir: c.u32()?, name: c.str()?, size: c.u64()?, mtime: c.i64()?, symlink: c.u8()? != 0 },
            3 => Op::Delete { file: c.u32()? },
            4 => Op::Read { file: c.u32()? },
            5 => Op::Write { file: c.u32()?, size: c.u64()?, mtime: c.i64()? },
            _ => return Err(bad("unknown op")),
        });
    }
    Ok(ops)
}

/// Writes a pre-built frame.
pub fn write_frame(w: &mut impl Write, frame: &[u8]) -> io::Result<()> {
    w.write_all(frame)
}

/// Reads one message (blocking).
pub fn read_msg(r: &mut impl Read) -> io::Result<Msg> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_le_bytes(head[..4].try_into().unwrap()) as usize;
    if len > MAX_FRAME {
        return Err(bad("frame too large"));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    let mut c = Cur { b: &payload, pos: 0 };
    match head[4] {
        K_HELLO => {
            if c.u32()? != MAGIC {
                return Err(bad("not a gits-collector socket"));
            }
            Ok(Msg::Hello { version: c.u16()? })
        }
        K_SNAPSHOT => Ok(Msg::Snapshot(payload)),
        K_OPS => Ok(Msg::Ops(decode_ops(&payload)?)),
        K_STATUS => Ok(Msg::Status(c.str()?)),
        _ => Err(bad("unknown frame kind")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Model {
        let mut m = Model::new("~", 1000);
        let a = m.add_dir(0, "a");
        let b = m.add_dir(a, "b");
        m.add_existing_file(a, "x.rs", 10, 5);
        m.add_existing_file(b, "y.png", 20, 6);
        m.add_existing_file(0, ".hidden", 1, 7);
        m.sort_all();
        m
    }

    #[test]
    fn snapshot_roundtrip_preserves_order_and_keys() {
        let m = sample();
        let d = decode_snapshot(&encode_snapshot(&m), 1000).unwrap();
        assert_eq!(d.dirs.len(), m.dirs.len());
        assert_eq!(d.files.len(), m.files.len());
        for (x, y) in m.dirs.iter().zip(&d.dirs) {
            assert_eq!((&x.name, x.parent, x.key, &x.files, &x.dirs), (&y.name, y.parent, y.key, &y.files, &y.dirs));
        }
        assert!(d.files[2].hidden);
    }

    #[test]
    fn ops_keep_ids_in_sync() {
        let mut a = sample();
        let mut b = decode_snapshot(&encode_snapshot(&a), 1000).unwrap();
        let ops = vec![
            Op::MkDir { parent: 1, name: "new".into() },
            Op::Create { dir: 3, name: "n.txt".into(), size: 3, mtime: 9, symlink: false },
            Op::Write { file: 3, size: 4, mtime: 10 },
            Op::Read { file: 0 },
            Op::Delete { file: 1 },
            Op::RmDir { dir: 2 },
        ];
        let frame = ops_frame(&ops);
        let decoded = match read_msg(&mut &frame[..]).unwrap() {
            Msg::Ops(o) => o,
            _ => panic!(),
        };
        assert_eq!(decoded, ops);
        for op in &ops {
            assert!(a.apply(op, 1.0));
            assert!(b.apply(op, 1.0));
        }
        assert_eq!(a.files.len(), b.files.len());
        assert_eq!(a.files[3].name, "n.txt");
        assert_eq!(b.files[3].size, 4);
        assert!(!b.is_live(1));
        assert!(b.dirs[2].removed);
    }

    #[test]
    fn rejects_unknown_ids() {
        let mut m = sample();
        assert!(!m.apply(&Op::Read { file: 999 }, 0.0));
        assert!(!m.apply(&Op::MkDir { parent: 999, name: "x".into() }, 0.0));
    }

    #[test]
    fn hello_and_status_frames() {
        let mut bytes = hello_frame();
        bytes.extend(status_frame("scanning"));
        let mut r = &bytes[..];
        assert!(matches!(read_msg(&mut r).unwrap(), Msg::Hello { version: VERSION }));
        assert!(matches!(read_msg(&mut r).unwrap(), Msg::Status(s) if s == "scanning"));
    }
}
