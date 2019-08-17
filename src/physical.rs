use fuse;

use time;

use self::fuse::{FileAttr, FileType};
use self::time::Timespec;
use std::ffi::OsStr;
use std::fs as stdfs;
use std::io::Result;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::PathBuf;

use crate::fs;

pub struct File {
    path: PathBuf,
}

impl File {
    pub fn new(path: PathBuf) -> File {
        File { path: path }
    }
}

impl fs::File for File {
    fn getattr(&self) -> Result<FileAttr> {
        stdfs::metadata(self.path.clone()).map(|m| to_fuse_file_attr(m))
    }
    fn open(&self) -> Result<Box<dyn fs::SeekableRead>> {
        Ok(Box::new(stdfs::File::open(&self.path)?))
    }
    fn name(&self) -> &OsStr {
        self.path.file_name().unwrap()
    }
}

pub struct Dir {
    path: PathBuf,
}

impl Dir {
    pub fn new(path: PathBuf) -> Self {
        Dir { path: path }
    }
}

impl fs::Dir for Dir {
    fn open(&self) -> Result<Box<dyn Iterator<Item = Result<fs::Entry>>>> {
        stdfs::read_dir(&self.path).map(|rd| -> Box<dyn Iterator<Item = Result<fs::Entry>>> {
            Box::new(DirHandler { iter: rd })
        })
    }
    fn lookup(&self, name: &OsStr) -> Result<fs::Entry> {
        let path = self.path.join(name);
        let m = stdfs::metadata(path.clone())?;
        if m.is_dir() {
            Ok(fs::Entry::Dir(Box::new(Dir::new(path))))
        } else {
            Ok(fs::Entry::File(Box::new(File::new(path))))
        }
    }
    fn getattr(&self) -> Result<FileAttr> {
        stdfs::metadata(self.path.clone()).map(|m| to_fuse_file_attr(m))
    }
    fn name(&self) -> &OsStr {
        self.path.file_name().unwrap()
    }
}

struct DirHandler {
    iter: stdfs::ReadDir,
}

fn to_fuse_entry<'a>(e: stdfs::DirEntry) -> fs::Entry {
    if e.file_type().unwrap().is_dir() {
        fs::Entry::Dir(Box::new(Dir::new(e.path())))
    } else {
        fs::Entry::File(Box::new(File::new(e.path())))
    }
}

impl Iterator for DirHandler {
    type Item = Result<fs::Entry>;

    fn next(&mut self) -> Option<Result<fs::Entry>> {
        self.iter.next().map(|r| r.map(|e| to_fuse_entry(e)))
    }
}

fn to_fuse_file_type(t: stdfs::FileType) -> FileType {
    if t.is_dir() {
        FileType::Directory
    } else if t.is_file() {
        FileType::RegularFile
    } else if t.is_symlink() {
        FileType::Symlink
    } else if t.is_block_device() {
        FileType::BlockDevice
    } else if t.is_char_device() {
        FileType::CharDevice
    } else if t.is_fifo() {
        FileType::NamedPipe
    } else {
        // socket is viewed as regular.
        FileType::RegularFile
    }
}

fn to_fuse_file_attr(m: stdfs::Metadata) -> FileAttr {
    FileAttr {
        ino: 0, // dummy
        size: m.size(),
        blocks: m.blocks(),
        atime: Timespec {
            sec: m.atime(),
            nsec: m.atime_nsec() as i32,
        },
        mtime: Timespec {
            sec: m.mtime(),
            nsec: m.mtime_nsec() as i32,
        },
        ctime: Timespec {
            sec: m.ctime(),
            nsec: m.ctime_nsec() as i32,
        },
        crtime: Timespec { sec: 0, nsec: 0 }, // mac only
        kind: to_fuse_file_type(m.file_type()),
        perm: m.permissions().mode() as u16,
        nlink: m.nlink() as u32,
        uid: m.uid(),
        gid: m.gid(),
        rdev: m.dev() as u32,
        flags: 0, // mac only
    }
}
