use fuse;
use libc;

use self::fuse::{FileAttr, FileType};
use std::cell::RefCell;
use std::collections::HashSet;
use std::convert::From;
use std::ffi::OsStr;
use std::io::{Error, Result};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::vec::Vec;

use crate::fs;
mod buffer;
mod link;
mod page;
mod reader;
mod wrapper;

fn to_fuse_file_type(file_type: libc::mode_t) -> FileType {
    match file_type & libc::S_IFMT {
        libc::S_IFLNK => FileType::Symlink,
        libc::S_IFREG => FileType::RegularFile,
        libc::S_IFBLK => FileType::BlockDevice,
        libc::S_IFDIR => FileType::Directory,
        libc::S_IFCHR => FileType::CharDevice,
        libc::S_IFIFO => FileType::NamedPipe,
        _ => FileType::RegularFile,
    }
}

fn to_fuse_file_attr(size: i64, file_type: libc::mode_t, attr: FileAttr) -> FileAttr {
    FileAttr {
        ino: 0, // dummy
        size: size as u64,
        blocks: (size as u64 + 4095) / 4096,
        atime: attr.atime,
        mtime: attr.mtime,
        ctime: attr.ctime,
        crtime: attr.crtime, // mac only
        kind: to_fuse_file_type(file_type),
        perm: attr.perm,
        nlink: 0,
        uid: attr.uid,
        gid: attr.gid,
        rdev: attr.rdev,
        flags: 0, // mac only
    }
}

struct ArchivedFile {
    archive: Rc<Box<dyn fs::File>>,
    attr: FileAttr,
    path: PathBuf,
}

impl ArchivedFile {
    fn new(archive: Rc<Box<dyn fs::File>>, attr: FileAttr, path: PathBuf) -> ArchivedFile {
        ArchivedFile {
            archive: archive,
            attr: attr,
            path: path,
        }
    }
}

impl fs::File for ArchivedFile {
    fn getattr(&self) -> Result<FileAttr> {
        Ok(self.attr)
    }

    fn open(&self) -> Result<Box<dyn fs::SeekableRead>> {
        let archive = wrapper::Archive::new(self.archive.open()?);
        let reader = archive
            .find_open(|e| e.pathname() == self.path)
            .unwrap_or(Err(Error::from_raw_os_error(libc::ENOENT)))?;
        Ok(Box::new(reader))
    }

    fn name(&self) -> &OsStr {
        self.path.file_name().unwrap()
    }
}

struct CacheFile {
    cache: RefCell<reader::Cache>,
    file: Rc<ArchivedFile>,
}

impl CacheFile {
    fn new(file: ArchivedFile, page_manager: Rc<RefCell<page::PageManager>>) -> CacheFile {
        let file = Rc::new(file);
        CacheFile {
            cache: RefCell::new(reader::Cache::new(page_manager, file.clone())),
            file: file,
        }
    }
}

impl fs::File for CacheFile {
    fn getattr(&self) -> Result<FileAttr> {
        self.file.getattr()
    }

    fn open(&self) -> Result<Box<dyn fs::SeekableRead>> {
        self.cache.borrow_mut().make_reader()
    }

    fn name(&self) -> &OsStr {
        self.file.name()
    }
}

struct DirEntry {
    attr: FileAttr,
    path: PathBuf,
}

pub struct Dir {
    archive: Rc<Box<dyn fs::File>>,
    path: PathBuf,
    attr: RefCell<Option<FileAttr>>,
    dents: RefCell<Option<Rc<Vec<DirEntry>>>>,
    page_manager: Rc<RefCell<page::PageManager>>,
}

impl Dir {
    pub fn new(f: Box<dyn fs::File>, page_manager: Rc<RefCell<page::PageManager>>) -> Self {
        Dir {
            archive: Rc::new(f),
            path: PathBuf::new(),
            attr: RefCell::new(None),
            dents: RefCell::new(None),
            page_manager: page_manager,
        }
    }

    fn from_parts(
        f: Rc<Box<dyn fs::File>>,
        path: PathBuf,
        attr: FileAttr,
        dents: Rc<Vec<DirEntry>>,
        page_manager: Rc<RefCell<page::PageManager>>,
    ) -> Self {
        Dir {
            archive: f,
            path: path,
            attr: RefCell::new(Some(attr)),
            dents: RefCell::new(Some(dents)),
            page_manager: page_manager,
        }
    }

    fn update_cache(&self) -> Result<()> {
        use crate::fs::Dir;
        if self.dents.borrow().is_some() {
            return Ok(());
        }
        let self_attr = self.getattr()?;
        let mut archive = wrapper::Archive::new(self.archive.open()?);
        let mut dents = Vec::new();
        let mut dirs = HashSet::new();
        loop {
            match archive.next_entry() {
                Some(Ok(ent)) => {
                    let path = ent.pathname();
                    let attr = to_fuse_file_attr(ent.size(), ent.filetype(), self_attr);
                    {
                        let mut parent = path.parent();
                        while parent.is_some() {
                            let path = parent.unwrap();
                            if dirs.insert(PathBuf::from(path)) {
                                dents.push(DirEntry {
                                    attr: self_attr,
                                    path: PathBuf::from(path),
                                });
                            }
                            parent = path.parent();
                        }
                    }
                    if attr.kind != FileType::Directory || dirs.insert(path.clone()) {
                        dents.push(DirEntry {
                            attr: attr,
                            path: path,
                        });
                    }
                }
                Some(Err(e)) => return Err(e),
                None => break,
            }
        }
        *self.dents.borrow_mut() = Some(Rc::new(dents));
        Ok(())
    }
}

impl fs::Dir for Dir {
    fn open(&self) -> Result<Box<dyn Iterator<Item = Result<fs::Entry>>>> {
        self.update_cache()?;
        Ok(Box::new(DirHandler::open(self)))
    }

    fn lookup(&self, name: &OsStr) -> Result<fs::Entry> {
        self.update_cache()?;
        let lookup_path = self.path.join(name);
        for e in self.dents.borrow().as_ref().unwrap().iter() {
            if e.path == lookup_path {
                if e.attr.kind == FileType::Directory {
                    return Ok(fs::Entry::Dir(Box::new(Dir::from_parts(
                        self.archive.clone(),
                        lookup_path.clone(),
                        e.attr,
                        self.dents.borrow().as_ref().unwrap().clone(),
                        self.page_manager.clone(),
                    ))));
                } else {
                    return Ok(fs::Entry::File(Box::new(CacheFile::new(
                        ArchivedFile::new(self.archive.clone(), e.attr, lookup_path.clone()),
                        self.page_manager.clone(),
                    ))));
                }
            }
        }
        Err(Error::from_raw_os_error(libc::ENOENT))
    }

    fn getattr(&self) -> Result<FileAttr> {
        if self.attr.borrow().is_none() {
            let mut attr = self.archive.getattr()?;
            attr.kind = FileType::Directory;
            *self.attr.borrow_mut() = Some(attr);
        }
        Ok(self.attr.borrow().unwrap())
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
    archive: Rc<Box<dyn fs::File>>,
    path: PathBuf,
    dents: Rc<Vec<DirEntry>>,
    i: usize,
    page_manager: Rc<RefCell<page::PageManager>>,
}

impl DirHandler {
    fn open(dir: &Dir) -> Self {
        DirHandler {
            archive: dir.archive.clone(),
            path: dir.path.clone(),
            dents: dir.dents.borrow().as_ref().unwrap().clone(),
            i: 0,
            page_manager: dir.page_manager.clone(),
        }
    }
}

impl Iterator for DirHandler {
    type Item = Result<fs::Entry>;

    fn next(&mut self) -> Option<Result<fs::Entry>> {
        let dents = self.dents.as_ref();
        while self.i < dents.len() {
            let e = &dents[self.i];
            self.i += 1;
            match e.path.parent() {
                Some(parent) if parent == self.path => {
                    if e.attr.kind == FileType::Directory {
                        let dir = Dir::from_parts(
                            self.archive.clone(),
                            e.path.clone(),
                            e.attr,
                            self.dents.clone(),
                            self.page_manager.clone(),
                        );
                        return Some(Ok(fs::Entry::Dir(Box::new(dir))));
                    } else {
                        let file = CacheFile::new(
                            ArchivedFile::new(self.archive.clone(), e.attr, e.path.clone()),
                            self.page_manager.clone(),
                        );
                        return Some(Ok(fs::Entry::File(Box::new(file))));
                    }
                }
                _ => continue,
            }
        }
        None
    }
}

pub struct ArchiveViewer {
    page_manager: Rc<RefCell<page::PageManager>>,
}

impl ArchiveViewer {
    pub fn new(max_bytes: usize) -> Result<ArchiveViewer> {
        wrapper::initialize();
        Ok(ArchiveViewer {
            page_manager: Rc::new(RefCell::new(page::PageManager::new(max_bytes)?)),
        })
    }
}

impl fs::Viewer for ArchiveViewer {
    fn view(&self, e: fs::Entry) -> fs::Entry {
        let is_archive = match e {
            fs::Entry::File(ref f) => {
                match Path::new(f.name()).extension().and_then(|ext| ext.to_str()) {
                    Some(ext) => match ext.to_lowercase().as_str() {
                        "zip" => true,
                        "rar" => true,
                        _ => false,
                    },
                    _ => false,
                }
            }
            _ => false,
        };
        if is_archive {
            if let fs::Entry::File(f) = e {
                return fs::Entry::Dir(Box::new(Dir::new(f, self.page_manager.clone())));
            }
        }
        e
    }
}

#[test]
fn test_iterate_dir() {
    use crate::fs::Dir as FSDir;
    use crate::physical;

    let page_manager = Rc::new(RefCell::new(
        page::PageManager::new(100 * 1024 * 1024).unwrap(),
    ));
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let zip = root.join("assets/test.zip");
    let zip_dir = Dir::new(Box::new(physical::File::new(zip)), page_manager.clone());
    let entries: Vec<_> = zip_dir.open().unwrap().map(|re| re.unwrap()).collect();
    assert!(entries
        .iter()
        .all(|e| { e.file_type(0).unwrap() == FileType::RegularFile }));
    let mut names: Vec<_> = entries.iter().map(|e| PathBuf::from(e.name())).collect();
    names.sort();
    let expect = vec![PathBuf::from("large"), PathBuf::from("small")];
    assert_eq!(names, expect);
}

#[test]
fn test_file_read() {
    use crate::fs::File;
    use crate::physical;
    use std::fs as stdfs;
    use std::io::Read;

    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    let zip = assets.join("test.zip");
    let zip_file = physical::File::new(zip);
    let read_archive = |name| {
        let archive = wrapper::Archive::new(zip_file.open().unwrap());
        let mut r = archive
            .find_open(|e| e.pathname() == PathBuf::from(name))
            .unwrap()
            .unwrap();
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
