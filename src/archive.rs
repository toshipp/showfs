extern crate fuse;
extern crate libarchive3_sys;
extern crate libarchive;
extern crate libc;

use self::fuse::{FileAttr, FileType};
use self::libarchive3_sys::ffi;
use self::libarchive::archive::{ReadFormat, Entry, FileType as AFileType};
use self::libarchive::error::ArchiveError;
use self::libarchive::reader::{Reader, StreamReader, ReaderEntry};
use std::cmp::min;
use std::convert::From;
use std::ffi::{CStr, OsStr};
use std::io::{Result, Error, Read, Seek, SeekFrom};
use std::path::{PathBuf, Path};
use std::rc::Rc;
use std::vec::Vec;

use fs;
use buffer::BufferedReader;

fn to_io_error(e: ArchiveError) -> Error {
    if let ArchiveError::Sys(code, _) = e {
        Error::from_raw_os_error(code.0)
    } else {
        Error::from_raw_os_error(libc::EIO)
    }
}

fn isdir(e: &ReaderEntry) -> bool {
    match e.filetype() {
        AFileType::Directory => true,
        _ => false,
    }
}

fn open(a: &fs::File) -> Result<StreamReader> {
    let builder = libarchive::reader::Builder::new();
    builder.support_format(ReadFormat::All).map_err(to_io_error)?;
    builder.open_stream(a.open()?).map_err(to_io_error)
}

fn pathname(e: &mut ReaderEntry) -> PathBuf {
    let c_str = unsafe { CStr::from_ptr(ffi::archive_entry_pathname(e.entry())) };
    PathBuf::from(c_str.to_string_lossy().as_ref())
}

fn find_entry(mut r: StreamReader, path: &PathBuf) -> Result<(StreamReader, bool)> {
    let exact;
    loop {
        match r.next_header() {
            Some(e) => {
                if let Ok(sub) = pathname(e).strip_prefix(path) {
                    exact = sub.as_os_str().is_empty();
                    break;
                }
            }
            None => return Err(Error::from_raw_os_error(libc::ENOENT)),
        }
    }
    Ok((r, exact))
}

fn to_fuse_file_type(t: AFileType) -> FileType {
    match t {
        AFileType::BlockDevice => FileType::BlockDevice,
        AFileType::CharacterDevice => FileType::CharDevice,
        AFileType::SymbolicLink => FileType::Symlink,
        AFileType::Directory => FileType::Directory,
        AFileType::NamedPipe => FileType::NamedPipe,
        _ => FileType::RegularFile,
    }
}

fn to_fuse_file_attr(e: &mut ReaderEntry, a: FileAttr) -> FileAttr {
    FileAttr {
        ino: 0, // dummy
        size: e.size() as u64,
        blocks: (e.size() as u64 + 4095) / 4096,
        atime: a.atime,
        mtime: a.mtime,
        ctime: a.ctime,
        crtime: a.crtime, // mac only
        kind: to_fuse_file_type(e.filetype()),
        perm: a.perm,
        nlink: 0,
        uid: a.uid,
        gid: a.gid,
        rdev: a.rdev,
        flags: 0, // mac only
    }
}

struct FileReader {
    r: StreamReader,
    data_offset: usize,
    read_pos: usize,
    data: Vec<u8>,
}

impl FileReader {
    fn new(r: StreamReader) -> Self {
        FileReader {
            r: r,
            data_offset: 0,
            read_pos: 0,
            data: Vec::new(),
        }
    }
}

impl Seek for FileReader {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        match pos {
            SeekFrom::Start(n) => {
                if (n as usize) < self.data_offset {
                    return Err(Error::from_raw_os_error(libc::EINVAL));
                }
                self.read_pos = n as usize;
                Ok(n)
            }
            _ => {
                // Not implemented
                return Err(Error::from_raw_os_error(libc::EINVAL));
            }
        }
    }
}

impl Read for FileReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.read_pos < self.data_offset {
            return Err(Error::from_raw_os_error(libc::EINVAL));
        }
        if self.read_pos >= self.data_offset + self.data.len() {
            self.data_offset += self.data.len();
            self.data.truncate(0);
            loop {
                match self.r.read_block() {
                    Ok(Some(block)) => {
                        debug!("block size {}", block.len());
                        if self.read_pos >= self.data_offset + block.len() {
                            self.data_offset += block.len();
                            continue;
                        }
                        self.data.extend_from_slice(block);
                        break;
                    }
                    Ok(None) => return Ok(0), //EOF
                    Err(e) => return Err(to_io_error(e)),
                };
            }
        }
        let begin = self.read_pos - self.data_offset;
        let l = min(self.data.len() - begin, buf.len());
        buf[..l].copy_from_slice(&self.data[begin..begin + l]);
        self.read_pos += l;
        Ok(l)
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
        let (mut r, _) = find_entry(open(&**self.archive)?, &self.path)?;
        Ok(to_fuse_file_attr(r.entry(), archive_attr))
    }

    fn open(&self) -> Result<Box<fs::SeekableRead>> {
        let (r, _) = find_entry(open(&**self.archive)?, &self.path)?;
        Ok(Box::new(BufferedReader::new(FileReader::new(r))))
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
        let (mut r, exact) = find_entry(open(&**self.archive)?, &lookup_path)?;
        if !exact || isdir(r.entry()) {
            Ok(fs::Entry::Dir(Box::new(Dir::new_for_path(self.archive.clone(), lookup_path))))
        } else {
            Ok(fs::Entry::File(Box::new(File::new(self.archive.clone(), lookup_path))))
        }
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
    r: StreamReader,
}

impl DirHandler {
    fn open(dir: &Dir) -> Result<Self> {
        Ok(DirHandler {
            archive: dir.archive.clone(),
            path: dir.path.clone(),
            dirs: Vec::new(),
            r: open(&**dir.archive)?,
        })
    }
}

impl Iterator for DirHandler {
    type Item = Result<fs::Entry>;

    fn next(&mut self) -> Option<Result<fs::Entry>> {
        while let Some(e) = self.r.next_header() {
            let path = pathname(e);
            debug!("pathname {:?}", path.as_path());
            if let Ok(sub) = path.strip_prefix(self.path.as_path()) {
                if sub.as_os_str().is_empty() {
                    continue;
                }
                let mut iter = sub.iter();
                let name = iter.next().unwrap();
                let isdir = iter.next().is_some() || isdir(e);
                if !isdir {
                    return Some(Ok(fs::Entry::File(Box::new(File::new(self.archive.clone(),
                                                                      self.path.join(name))))));
                }
                if self.dirs.iter().find(|n| n.as_os_str() == name).is_some() {
                    continue;
                }
                self.dirs.push(PathBuf::from(name));
                return Some(Ok(fs::Entry::Dir(Box::new(Dir::new_for_path(self.archive.clone(),
                                                                         self.path.join(name))))));
            }
        }
        None
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
        let is_archive = match Path::new(f.name()).extension() {
            Some(ext) if ext == "zip" || ext == "rar" => true,
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
    let mut names: Vec<PathBuf> =
        zip_dir.open().unwrap().map(|re| PathBuf::from(re.unwrap().name())).collect();
    names.sort();
    let expect = vec![PathBuf::from("large"), PathBuf::from("small")];
    assert_eq!(names, expect);
}

#[test]
fn test_file_read() {
    use std::fs as stdfs;
    use physical;

    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    let zip = assets.join("test.zip");
    let zip_file = physical::File::new(zip);
    let open_archive = |name| {
        let mut v = Vec::<u8>::new();
        let mut r =
            FileReader::new(find_entry(open(&zip_file).unwrap(), &PathBuf::from(name)).unwrap().0);
        r.read_to_end(&mut v).unwrap();
        v
    };
    let open_file = |name| {
        let mut v = Vec::<u8>::new();
        let mut r = stdfs::File::open(assets.join(name)).unwrap();
        r.read_to_end(&mut v).unwrap();
        v
    };

    let small_actual = open_archive("small");
    let small_expect = open_file("small");
    assert_eq!(small_actual, small_expect);

    let large_actual = open_archive("large");
    let large_expect = open_file("large");
    assert_eq!(large_actual, large_expect);
}
