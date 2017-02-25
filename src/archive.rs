extern crate fuse;
extern crate libc;

use self::fuse::{FileAttr, FileType};
use std::convert::From;
use std::ffi::OsStr;
use std::io::{Result, Error};
use std::path::{PathBuf, Path};
use std::rc::Rc;
use std::vec::Vec;

use fs;
use fs::SeekableRead;
use wrapper;
use buffer;

fn isdir(m: libc::mode_t) -> bool {
    m & libc::S_IFDIR != 0
}

fn to_fuse_file_type(m: libc::mode_t) -> FileType {
    match m & libc::S_IFMT {
        libc::S_IFLNK => FileType::Symlink,
        libc::S_IFREG => FileType::RegularFile,
        libc::S_IFBLK => FileType::BlockDevice,
        libc::S_IFDIR => FileType::Directory,
        libc::S_IFCHR => FileType::CharDevice,
        libc::S_IFIFO => FileType::NamedPipe,
        _ => FileType::RegularFile,
    }
}

fn to_fuse_file_attr(s: libc::int64_t, m: libc::mode_t, a: FileAttr) -> FileAttr {
    FileAttr {
        ino: 0, // dummy
        size: s as u64,
        blocks: (s as u64 + 4095) / 4096,
        atime: a.atime,
        mtime: a.mtime,
        ctime: a.ctime,
        crtime: a.crtime, // mac only
        kind: to_fuse_file_type(m),
        perm: a.perm,
        nlink: 0,
        uid: a.uid,
        gid: a.gid,
        rdev: a.rdev,
        flags: 0, // mac only
    }
}

struct File {
    archive: Rc<Box<fs::File>>,
    path: PathBuf,
}

impl File {
    fn new(archive: Rc<Box<fs::File>>, path: PathBuf) -> File {
        File {
            archive: archive,
            path: path,
        }
    }
}

impl fs::File for File {
    fn getattr(&self) -> Result<FileAttr> {
        let archive_attr = self.archive.getattr()?;
        let archive = wrapper::Archive::new(self.archive.open(false)?);
        archive.find_map(|e| if e.pathname() == self.path {
                Some(to_fuse_file_attr(e.size(), e.filetype(), archive_attr))
            } else {
                None
            })
            .unwrap_or(Err(Error::from_raw_os_error(libc::ENOENT)))
    }

    fn open(&self, need_bidirectional: bool) -> Result<Box<fs::SeekableRead>> {
        let archive = wrapper::Archive::new(self.archive.open(false)?);
        let reader = archive.find_open(|e| e.pathname() == self.path)
            .unwrap_or(Err(Error::from_raw_os_error(libc::ENOENT)))?;
        if need_bidirectional {
            Ok(Box::new(buffer::BufferedReader::new(reader)))
        } else {
            Ok(Box::new(reader))
        }
    }

    fn name(&self) -> &OsStr {
        self.path.file_name().unwrap()
    }
}

pub struct Dir {
    archive: Rc<Box<fs::File>>,
    path: PathBuf,
}

impl Dir {
    pub fn new(f: Box<fs::File>) -> Self {
        Dir::new_for_path(Rc::new(f), PathBuf::new())
    }
    fn new_for_path(f: Rc<Box<fs::File>>, path: PathBuf) -> Self {
        Dir {
            archive: f,
            path: path,
        }
    }
}

impl fs::Dir for Dir {
    fn open(&self) -> Result<Box<Iterator<Item = Result<fs::Entry>>>> {
        Ok(Box::new(DirHandler::open(self)?))
    }
    fn lookup(&self, name: &OsStr) -> Result<fs::Entry> {
        let lookup_path = self.path.join(name);
        let archive = wrapper::Archive::new(self.archive.open(false)?);
        archive.find_map(|e| if let Ok(sub) = e.pathname().strip_prefix(lookup_path.as_path()) {
                if sub.as_os_str().is_empty() && !isdir(e.filetype()) {
                    Some(fs::Entry::File(Box::new(File::new(self.archive.clone(),
                                                            lookup_path.clone()))))
                } else {
                    Some(fs::Entry::Dir(Box::new(Dir::new_for_path(self.archive.clone(),
                                                                   lookup_path.clone()))))
                }
            } else {
                None
            })
            .unwrap_or(Err(Error::from_raw_os_error(libc::ENOENT)))
    }
    fn getattr(&self) -> Result<FileAttr> {
        self.archive.getattr().map(|mut attr| {
            attr.kind = FileType::Directory;
            attr
        })
    }
    fn name(&self) -> &OsStr {
        if self.path.as_os_str().is_empty() {
            self.archive.name()
        } else {
            self.path.file_name().unwrap()
        }
    }
}

struct DirHandler {
    archive: Rc<Box<fs::File>>,
    path: PathBuf,
    dirs: Vec<PathBuf>,
    a: wrapper::Archive<Box<SeekableRead>>,
}

impl DirHandler {
    fn open(dir: &Dir) -> Result<Self> {
        Ok(DirHandler {
            archive: dir.archive.clone(),
            path: dir.path.clone(),
            dirs: Vec::new(),
            a: wrapper::Archive::new(dir.archive.open(false)?),
        })
    }
}

impl Iterator for DirHandler {
    type Item = Result<fs::Entry>;

    fn next(&mut self) -> Option<Result<fs::Entry>> {
        loop {
            match self.a.next_entry() {
                Some(Ok(e)) => {
                    let path = e.pathname();
                    debug!("pathname {:?}", path);
                    if let Ok(sub) = path.strip_prefix(self.path.as_path()) {
                        if sub.as_os_str().is_empty() {
                            continue;
                        }
                        let mut iter = sub.iter();
                        let name = iter.next().unwrap();
                        let isdir = iter.next().is_some() || isdir(e.filetype());
                        if !isdir {
                            return Some(Ok(fs::Entry::File(Box::new(File::new(self.archive
                                                                                  .clone(),
                                                                              self.path
                                                                                  .join(name))))));
                        }
                        if self.dirs.iter().find(|n| n.as_os_str() == name).is_some() {
                            continue;
                        }
                        self.dirs.push(PathBuf::from(name));
                        return Some(Ok(fs::Entry::Dir(Box::new(Dir::new_for_path(
                            self.archive.clone(),
                            self.path.join(name))))));
                    }
                }
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            }
        }
    }
}

fn file_to_archive(e: fs::Entry) -> fs::Entry {
    if let fs::Entry::File(f) = e {
        return fs::Entry::Dir(Box::new(Dir::new(f)));
    }
    panic!("invalid entry");
}

pub fn view_archive(e: &fs::Entry) -> Option<Box<Fn(fs::Entry) -> fs::Entry>> {
    if let &fs::Entry::File(ref f) = e {
        let is_archive = match Path::new(f.name()).extension().and_then(|ext| ext.to_str()) {
            Some(ext) => {
                match ext.to_lowercase().as_str() {
                    "zip" => true,
                    "rar" => true,
                    _ => false,
                }
            }
            _ => false,
        };
        if is_archive {
            return Some(Box::new(file_to_archive));
        }
    }
    None
}

#[test]
fn test_iterate_dir() {
    use physical;
    use fs::Dir as FSDir;

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let zip = root.join("assets/test.zip");
    let zip_dir = Dir::new(Box::new(physical::File::new(zip)));
    let entries: Vec<_> = zip_dir.open().unwrap().map(|re| re.unwrap()).collect();
    assert!(entries.iter().all(|e| e.file_type(0).unwrap() == FileType::RegularFile));
    let mut names: Vec<_> = entries.iter().map(|e| PathBuf::from(e.name())).collect();
    names.sort();
    let expect = vec![PathBuf::from("large"), PathBuf::from("small")];
    assert_eq!(names, expect);
}

#[test]
fn test_file_read() {
    use std::fs as stdfs;
    use fs::File;
    use physical;
    use std::io::Read;

    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    let zip = assets.join("test.zip");
    let zip_file = physical::File::new(zip);
    let read_archive = |name| {
        let archive = wrapper::Archive::new(zip_file.open(false).unwrap());
        let mut r = archive.find_open(|e| e.pathname() == PathBuf::from(name)).unwrap().unwrap();
        let mut v = Vec::<u8>::new();
        r.read_to_end(&mut v).unwrap();
        v
    };
    let read_file = |name| {
        let mut v = Vec::<u8>::new();
        let mut r = stdfs::File::open(assets.join(name)).unwrap();
        r.read_to_end(&mut v).unwrap();
        v
    };

    let small_actual = read_archive("small");
    let small_expect = read_file("small");
    assert_eq!(small_actual, small_expect);

    let large_actual = read_archive("large");
    let large_expect = read_file("large");
    assert_eq!(large_actual, large_expect);
}
