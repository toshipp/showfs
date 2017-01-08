extern crate libc;
extern crate libarchive;
extern crate fuse;
extern crate time;

use self::fuse::{Filesystem, Request, ReplyData, ReplyEntry, ReplyAttr, ReplyDirectory, ReplyOpen,
                 ReplyEmpty, FileAttr, FileType};
use self::time::Timespec;
use std::collections::HashMap;
use std::convert::AsRef;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::io::{Result, Error, ErrorKind};
use std::iter;
use std::path::{Path, PathBuf};
use std::vec::Vec;
use std::rc::Rc;

use physical;

macro_rules! error_with_log {
    ($reply:expr, $e:expr) => {{
        debug!("{}:{}: {:?}", file!(), line!(), $e);
        $reply.error(to_cerr($e))
    }}
}

// TODO: configurable?
const TTL: Timespec = Timespec { sec: 1, nsec: 0 };

pub trait SeekableRead: Seek + Read {}
impl<T: Seek + Read> SeekableRead for T {}

pub enum Entry {
    File(Box<File>),
    Dir(Box<Dir>),
}

impl Entry {
    pub fn getattr(&self, ino: u64) -> Result<FileAttr> {
        let attr = match self {
            &Entry::File(ref f) => f.getattr(),
            &Entry::Dir(ref d) => d.getattr(),
        };
        attr.map(|mut a| {
            a.ino = ino;
            a
        })
    }
    pub fn name(&self) -> &OsStr {
        match self {
            &Entry::File(ref f) => f.name(),
            &Entry::Dir(ref d) => d.name(),
        }
    }
    pub fn file_type(&self, ino: u64) -> Result<FileType> {
        self.getattr(ino).map(|a| a.kind)
    }
}

pub trait File {
    fn getattr(&self) -> Result<FileAttr>;
    fn open(&self) -> Result<Box<SeekableRead>>;
    fn name(&self) -> &OsStr;
}

pub trait Dir {
    fn open(&self) -> Result<Box<Iterator<Item = Result<Entry>>>>;
    fn lookup(&self, name: &Path) -> Result<Entry>;
    fn getattr(&self) -> Result<FileAttr>;
    fn name(&self) -> &OsStr;
}

fn to_cerr(e: Error) -> libc::c_int {
    match e.raw_os_error() {
        Some(raw) => raw,
        None => libc::EIO,
    }
}

struct InodeReserver {
    inode: u64,
}

impl InodeReserver {
    fn inode(&self) -> u64 {
        return self.inode;
    }
}

struct EntryHolder {
    inode: u64,
    inode_to_entry: HashMap<u64, Entry>,
    path_to_inode: HashMap<(u64, OsString), u64>,
}

impl EntryHolder {
    fn new() -> EntryHolder {
        EntryHolder {
            inode: 0,
            inode_to_entry: HashMap::new(),
            path_to_inode: HashMap::new(),
        }
    }
    fn get_by_path(&self, parent: u64, name: &OsStr) -> Option<(u64, &Entry)> {
        self.path_to_inode
            .get(&(parent, name.to_os_string()))
            .and_then(|ino| self.inode_to_entry.get(ino).map(|e| (*ino, e)))
    }
    fn reserve_inode(&mut self) -> InodeReserver {
        let i = self.inode;
        self.inode += 1;
        InodeReserver { inode: i }
    }
    fn register_with(&mut self, parent: u64, ent: Entry, ir: InodeReserver) {
        debug!("register {:?} with {}", ent.name(), ir.inode);
        self.path_to_inode.insert((parent, ent.name().to_os_string()), ir.inode);
        self.inode_to_entry.insert(ir.inode, ent);
    }
    fn register_root(&mut self, root: Entry) {
        self.inode = 2; // next to root (1)
        self.register_with(0, root, InodeReserver { inode: 1 })
    }
    fn get_by_inode(&self, ino: u64) -> Option<&Entry> {
        self.inode_to_entry.get(&ino)
    }
}

struct HandlerHolder {
    fh: u64, // fh counter
    file_handlers: HashMap<u64, Box<SeekableRead>>,
    dir_handlers: HashMap<u64, iter::Peekable<Box<Iterator<Item = Result<Entry>>>>>,
}

impl HandlerHolder {
    fn new() -> HandlerHolder {
        HandlerHolder {
            fh: 0,
            file_handlers: HashMap::new(),
            dir_handlers: HashMap::new(),
        }
    }
    fn register_file(&mut self, r: Box<SeekableRead>) -> u64 {
        let fh = self.fh;
        self.fh += 1;
        self.file_handlers.insert(fh, r);
        return fh;
    }
    fn register_dir<I>(&mut self, iter: I) -> u64
        where I: Iterator<Item = Result<Entry>> + 'static
    {
        let fh = self.fh;
        self.fh += 1;
        let iter: Box<Iterator<Item = Result<Entry>>> = Box::new(iter);
        self.dir_handlers.insert(fh, iter.peekable());
        return fh;
    }
    fn get_file(&self, fh: u64) -> Option<&Box<SeekableRead>> {
        self.file_handlers.get(&fh)
    }
    fn get_file_mut(&mut self, fh: u64) -> Option<&mut Box<SeekableRead>> {
        self.file_handlers.get_mut(&fh)
    }
    fn get_dir_mut(&mut self,
                   fh: u64)
                   -> Option<&mut iter::Peekable<Box<Iterator<Item = Result<Entry>>>>> {
        self.dir_handlers.get_mut(&fh)
    }
    fn release_file(&mut self, fh: u64) {
        self.file_handlers.remove(&fh);
    }
    // if the handler is not found, return false.
    fn release_dir(&mut self, fh: u64) -> bool {
        self.dir_handlers.remove(&fh).is_some()
    }
}

#[derive(Clone)]
struct Viewer {
    viewers: Rc<Vec<Box<Fn(&Entry) -> Option<Box<Fn(Entry) -> Entry>>>>>,
}

impl Viewer {
    fn new() -> Viewer {
        Viewer { viewers: Rc::new(Vec::new()) }
    }

    fn register<F>(&mut self, view: F)
        where F: Fn(&Entry) -> Option<Box<Fn(Entry) -> Entry>> + 'static
    {
        Rc::get_mut(&mut self.viewers).unwrap().push(Box::new(view))
    }

    fn viewed_as(&self, e: Entry) -> Entry {
        for ref checker in self.viewers.iter() {
            if let Some(view) = checker(&e) {
                return view(e);
            }
        }
        e
    }
}

pub struct ShowFS {
    origin: PathBuf,
    entries: EntryHolder,
    handlers: HandlerHolder,
    viewer: Viewer,
    buf: Vec<u8>,
}

impl ShowFS {
    pub fn new<P>(origin: P) -> ShowFS
        where P: AsRef<Path>
    {
        ShowFS {
            origin: origin.as_ref().to_path_buf(),
            entries: EntryHolder::new(),
            handlers: HandlerHolder::new(),
            viewer: Viewer::new(),
            buf: Vec::new(),
        }
    }

    pub fn register_viewer<F>(&mut self, viewer: F) -> &mut ShowFS
        where F: Fn(&Entry) -> Option<Box<Fn(Entry) -> Entry>> + 'static
    {
        self.viewer.register(viewer);
        self
    }

    pub fn mount<P>(mut self, target: P) -> Result<()>
        where P: AsRef<Path>
    {
        let root = if fs::metadata(self.origin.clone())?.is_dir() {
            Entry::Dir(Box::new(physical::Dir::new(self.origin.clone())))
        } else {
            Entry::File(Box::new(physical::File::new(self.origin.clone())))
        };
        let viewed_root = self.viewer.viewed_as(root);
        match viewed_root {
            Entry::Dir(_) if fs::metadata(target.as_ref())?.is_dir() => {
                // fallthrough
            }
            _ => {
                return Err(Error::new(ErrorKind::InvalidInput, "invalid origin or mountpoint"));
            }
        }
        self.entries.register_root(viewed_root);
        Ok(fuse::mount(self, &target, &[]))
    }
}

impl Filesystem for ShowFS {
    // kernel path resolving function
    fn lookup(&mut self, _req: &Request, parent: u64, name: &Path, reply: ReplyEntry) {
        // check cache.
        match self.entries.get_by_path(parent, name.as_os_str()) {
            Some((ino, ent)) => {
                match ent.getattr(ino) {
                    Ok(attr) => {
                        reply.entry(&TTL, &attr, 0);
                        return;
                    }
                    Err(e) => {
                        error_with_log!(reply, e);
                        return;
                    }
                }
            }
            _ => {
                // fallthrough
            }
        }

        // look underlying.
        let ret_ent = match self.entries.get_by_inode(parent) {
            Some(&Entry::Dir(ref p)) => p.lookup(name),
            _ => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        let attr = match ret_ent {
            Ok(ent) => {
                let ir = self.entries.reserve_inode();
                let ent = self.viewer.viewed_as(ent);
                let attr = ent.getattr(ir.inode());
                self.entries.register_with(parent, ent, ir);
                attr
            }
            Err(e) => {
                error_with_log!(reply, e);
                return;
            }
        };
        match attr {
            Ok(attr) => reply.entry(&TTL, &attr, 0),
            Err(e) => error_with_log!(reply, e),
        }
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        if let Some(ent) = self.entries.get_by_inode(ino) {
            match ent.getattr(ino) {
                Ok(attr) => reply.attr(&TTL, &attr),
                Err(e) => error_with_log!(reply, e),
            }
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn open(&mut self, _req: &Request, ino: u64, flags: u32, reply: ReplyOpen) {
        if flags & libc::O_RDONLY as u32 != 0 {
            // support read only.
            reply.error(libc::EINVAL);
            return;
        }

        let file = match self.entries.get_by_inode(ino) {
            Some(&Entry::File(ref file)) => file.clone(),
            Some(_) => {
                reply.error(libc::EINVAL);
                return;
            }
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        match file.open() {
            Ok(contents) => {
                let fh = self.handlers.register_file(contents);
                // flag can only be direct_io or keep_cache.
                reply.opened(fh, 0);
            }
            Err(e) => error_with_log!(reply, e),
        }
    }

    // called when all opened fds are closed.
    fn release(&mut self,
               _req: &Request,
               _ino: u64,
               fh: u64,
               _flags: u32,
               _lock_owner: u64,
               _flush: bool,
               reply: ReplyEmpty) {
        if self.handlers.get_file(fh).is_none() {
            reply.error(libc::EBADF);
            return;
        }
        self.handlers.release_file(fh);
        reply.ok();
    }

    fn read(&mut self,
            _req: &Request,
            _ino: u64,
            fh: u64,
            offset: u64,
            size: u32,
            reply: ReplyData) {
        if let Some(reader) = self.handlers.get_file_mut(fh) {
            if let Err(e) = reader.seek(SeekFrom::Start(offset)) {
                error_with_log!(reply, e);
                return;

            }
            let size = size as usize;
            self.buf.resize(size, 0);
            let mut read = 0;
            while read < size {
                match reader.read(&mut self.buf[read..]) {
                    Ok(n) if n == 0 => break,
                    Ok(n) => read += n,
                    Err(e) => {
                        error_with_log!(reply, e);
                        return;
                    }
                }
            }
            reply.data(&self.buf[..read])
        } else {
            reply.error(libc::EBADF)
        }
    }

    fn opendir(&mut self, _req: &Request, ino: u64, _flags: u32, reply: ReplyOpen) {
        let handler = match self.entries.get_by_inode(ino) {
            Some(&Entry::Dir(ref d)) => d.open(),
            Some(_) => {
                reply.error(libc::EBADF);
                return;
            }
            None => {
                reply.error(libc::ENOENT);
                return;
            }

        };
        match handler {
            Ok(dh) => {
                let viewer = self.viewer.clone();
                let fh = self.handlers
                    .register_dir(dh.map(move |re| re.map(|e| viewer.viewed_as(e))));
                reply.opened(fh, 0);
            }
            Err(e) => error_with_log!(reply, e),
        }
    }

    fn releasedir(&mut self, _req: &Request, _ino: u64, fh: u64, _flags: u32, reply: ReplyEmpty) {
        if self.handlers.release_dir(fh) {
            reply.ok();
        } else {
            reply.error(libc::EBADF);
            return;
        }
    }

    fn readdir(&mut self,
               _req: &Request,
               ino: u64,
               fh: u64,
               offset: u64,
               mut reply: ReplyDirectory) {
        let h = match self.handlers.get_dir_mut(fh) {
            Some(h) => h,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        for offset in (offset + 1).. {
            let mut reserver = None;
            // check if an entry can be inserted.
            match h.peek() {
                Some(&Ok(ref ent)) => {
                    let ent_ino = match self.entries.get_by_path(ino, ent.name()) {
                        Some((ent_ino, _)) => ent_ino,
                        None => {
                            let r = self.entries.reserve_inode();
                            let i = r.inode();
                            reserver = Some(r);
                            i
                        }
                    };
                    match ent.file_type(ent_ino) {
                        Ok(ft) => {
                            if reply.add(ent_ino, offset, ft, ent.name()) {
                                // buffer is full.
                                reply.ok();
                                return;
                            }
                        }
                        Err(e) => {
                            error_with_log!(reply, e);
                            return;
                        }
                    }
                }
                _ => {
                    // fallthrough
                }
            }

            match h.next() {
                Some(Ok(ent)) => {
                    if let Some(r) = reserver {
                        self.entries.register_with(ino, ent, r)
                    }
                }
                Some(Err(e)) => {
                    error_with_log!(reply, e);
                    return;
                }
                None => {
                    reply.ok();
                    return;
                }
            }
        }
    }
}
