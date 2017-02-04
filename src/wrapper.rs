extern crate libc;
extern crate libarchive3_sys;

use self::libarchive3_sys::ffi;
use std::ffi::{CStr, CString};
use std::marker;
use std::ptr;
use std::io::{Result, Error, SeekFrom, Read, Seek, ErrorKind};
use std::error::Error as STDError;
use fs::{SeekableRead, SeekExt};
use std::cmp::min;
use std::path::PathBuf;
use std::sync::{Once, ONCE_INIT};

// libarchive needs locale to convert pathname.
fn setlocale_once() {
    static ONCE: Once = ONCE_INIT;
    ONCE.call_once(|| unsafe {
        libc::setlocale(libc::LC_ALL, CString::new("").unwrap().as_ptr());
    });
}

struct Proxy<R: SeekableRead> {
    r: R,
    buf: Vec<u8>,
    pos: u64,
}

impl<R: SeekableRead> Proxy<R> {
    fn new(r: R) -> Proxy<R> {
        let mut v = Vec::new();
        v.resize(4096, 0);
        Proxy {
            r: r,
            buf: v,
            pos: 0,
        }
    }

    fn read(&mut self) -> Result<&[u8]> {
        let n = self.r.read(&mut self.buf[..])?;
        self.pos += n as u64;
        Ok(&self.buf[..n])
    }

    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        self.pos = self.r.seek(pos)?;
        Ok(self.pos)
    }

    fn skip(&mut self, request: libc::int64_t) -> Result<libc::int64_t> {
        let now = self.r.seek(SeekFrom::Current(request))?;
        let skip = now - self.pos;
        self.pos = now;
        Ok(skip as libc::int64_t)
    }
}

pub struct Archive<R: SeekableRead> {
    raw: *mut ffi::Struct_archive,
    eof: bool,
    _proxy: Box<Proxy<R>>,
}

unsafe fn set_error(raw: *mut ffi::Struct_archive, e: Error) {
    let desc = CString::new(e.description()).unwrap();
    ffi::archive_set_error(raw, e.raw_os_error().unwrap_or(libc::EIO), desc.as_ptr());
}

unsafe fn error_string(raw: *mut ffi::Struct_archive) -> String {
    CStr::from_ptr(ffi::archive_error_string(raw)).to_str().unwrap().to_string()
}

unsafe extern "C" fn read_callback<R: SeekableRead>(raw: *mut ffi::Struct_archive,
                                                    client_data: *mut libc::c_void,
                                                    buffer: *mut *const libc::c_void)
                                                    -> libc::ssize_t {
    let proxy = (client_data as *mut Proxy<R>).as_mut().unwrap();
    let err;
    match proxy.read() {
        Ok(data) => {
            *buffer = data.as_ptr() as *const libc::c_void;
            return data.len() as libc::ssize_t;
        }
        Err(e) => err = e,
    };
    set_error(raw, err);
    -1
}

unsafe extern "C" fn skip_callback<R: SeekableRead>(raw: *mut ffi::Struct_archive,
                                                    client_data: *mut libc::c_void,
                                                    request: libc::int64_t)
                                                    -> libc::int64_t {
    let proxy = (client_data as *mut Proxy<R>).as_mut().unwrap();
    match proxy.skip(request) {
        Ok(n) => n,
        Err(e) => {
            // collect?
            set_error(raw, e);
            0
        }
    }
}

unsafe extern "C" fn seek_callback<R: SeekableRead>(raw: *mut ffi::Struct_archive,
                                                    client_data: *mut libc::c_void,
                                                    offset: libc::int64_t,
                                                    whence: libc::c_int)
                                                    -> libc::int64_t {
    let proxy = (client_data as *mut Proxy<R>).as_mut().unwrap();
    let pos = match whence {
        libc::SEEK_SET => SeekFrom::Start(offset as u64),
        libc::SEEK_CUR => SeekFrom::Current(offset),
        libc::SEEK_END => SeekFrom::End(offset),
        _ => unreachable!(),
    };
    match proxy.seek(pos) {
        Ok(n) => n as libc::int64_t,
        Err(e) => {
            // collect?
            set_error(raw, e);
            -1
        }
    }
}

impl<R: SeekableRead> Archive<R> {
    pub fn new(r: R) -> Self {
        setlocale_once();
        unsafe {
            let raw = ffi::archive_read_new();
            if raw.is_null() {
                panic!("oom");
            }
            if ffi::archive_read_support_format_all(raw) != ffi::ARCHIVE_OK {
                panic!("not support format");
            }
            if ffi::archive_read_support_filter_all(raw) != ffi::ARCHIVE_OK {
                panic!("not support filter");
            }
            if r.bidirectional() {
                if ffi::archive_read_set_seek_callback(raw, Some(seek_callback::<R>)) !=
                   ffi::ARCHIVE_OK {
                    panic!("failed to set seek");
                }
            } else {
                if ffi::archive_read_set_skip_callback(raw, Some(skip_callback::<R>)) !=
                   ffi::ARCHIVE_OK {
                    panic!("failed to set skip");
                }
            }
            let proxy = Box::into_raw(Box::new(Proxy::new(r)));
            if ffi::archive_read_open(raw,
                                      proxy as *mut libc::c_void,
                                      None,
                                      Some(read_callback::<R>),
                                      None) != ffi::ARCHIVE_OK {
                panic!("failed to open");
            }
            Archive {
                raw: raw,
                eof: false,
                _proxy: Box::from_raw(proxy),
            }
        }
    }

    fn next_entry_raw(&mut self) -> Option<Result<Entry>> {
        if self.eof {
            return None;
        }

        let mut entry = ptr::null_mut();
        loop {
            match unsafe { ffi::archive_read_next_header(self.raw, &mut entry) } {
                ffi::ARCHIVE_OK => break,
                ffi::ARCHIVE_WARN => {
                    warn!("archive_read_next_header: {}",
                          unsafe { error_string(self.raw) });
                    break;
                }
                ffi::ARCHIVE_EOF => {
                    self.eof = true;
                    return None;
                }
                ffi::ARCHIVE_RETRY => {
                    // failed but retryable.
                    warn!("archive_read_next_header: {}, retry.",
                          unsafe { error_string(self.raw) });
                    continue;
                }
                ffi::ARCHIVE_FATAL => {
                    return Some(Err(Error::new(ErrorKind::Other,
                                               unsafe { error_string(self.raw) })));
                }
                _ => unreachable!(),
            }

        }
        Some(Ok(Entry::new(entry)))
    }

    pub fn next_entry<'a>(&'a mut self) -> Option<Result<RefEntry<'a, R>>> {
        self.next_entry_raw().map(|r| r.map(|e| RefEntry::new(e)))
    }

    pub fn find_map<F, T>(mut self, f: F) -> Option<Result<T>>
        where F: Fn(&Entry) -> Option<T>
    {
        loop {
            match self.next_entry_raw() {
                Some(Ok(e)) => {
                    if let Some(x) = f(&e) {
                        return Some(Ok(x));
                    }
                }
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            }
        }
    }

    pub fn find_open<P>(mut self, p: P) -> Option<Result<Reader<R>>>
        where P: Fn(&Entry) -> bool
    {
        loop {
            match self.next_entry_raw() {
                Some(Ok(e)) => {
                    if p(&e) {
                        break;
                    }
                }
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            }
        }
        Some(Ok(Reader::new(self)))
    }
}

impl<R: SeekableRead> Drop for Archive<R> {
    fn drop(&mut self) {
        unsafe { ffi::archive_read_free(self.raw) };
    }
}

pub struct Reader<R: SeekableRead> {
    a: Archive<R>,
    buf: *const libc::c_void,
    read_pos: usize,
    buf_size: libc::size_t,
    offset: libc::off_t,
    eof: bool,
}

impl<R: SeekableRead> Reader<R> {
    fn new(a: Archive<R>) -> Reader<R> {
        Reader {
            a: a,
            buf: ptr::null(),
            read_pos: 0,
            buf_size: 0,
            offset: 0,
            eof: false,
        }
    }

    fn fill_gap(&mut self, buf: &mut [u8]) -> usize {
        if self.read_pos < self.offset as usize {
            let l = min(buf.len(), (self.offset as usize) - self.read_pos);
            for x in &mut buf[..l] {
                *x = 0;
            }
            self.read_pos += l;
            return l;
        }
        0
    }

    fn read_data_block(&mut self) -> Result<()> {
        if self.eof {
            return Ok(());
        }

        while self.offset as usize + self.buf_size as usize <= self.read_pos {
            match unsafe {
                ffi::archive_read_data_block(self.a.raw,
                                             &mut self.buf,
                                             &mut self.buf_size,
                                             &mut self.offset)
            } {
                ffi::ARCHIVE_OK => continue,
                ffi::ARCHIVE_WARN => {
                    warn!("archive_read_data_block: {}",
                          unsafe { error_string(self.a.raw) });
                    continue;
                }
                ffi::ARCHIVE_EOF => {
                    self.eof = true;
                    return Ok(());
                }
                ffi::ARCHIVE_RETRY => {
                    // failed but retryable.
                    warn!("archive_read_data_block: {}, retry",
                          unsafe { error_string(self.a.raw) });
                    continue;
                }
                ffi::ARCHIVE_FATAL => {
                    return Err(Error::new(ErrorKind::Other, unsafe { error_string(self.a.raw) }));
                }
                _ => unreachable!(),
            }
        }
        Ok(())
    }
}

impl<R: SeekableRead> SeekExt for Reader<R> {
    fn bidirectional(&self) -> bool {
        return false;
    }
}

impl<R: SeekableRead> Read for Reader<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.read_data_block()?;
        let n = self.fill_gap(buf);
        if n > 0 {
            return Ok(n);
        }
        let begin = self.read_pos - self.offset as usize;
        let l = min(buf.len(), self.buf_size - begin);
        unsafe {
            let p = (self.buf as *const u8).offset(begin as isize);
            ptr::copy_nonoverlapping(p, buf.as_mut_ptr(), l);
        }
        self.read_pos += l;
        Ok(l)
    }
}

impl<R: SeekableRead> Seek for Reader<R> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        match pos {
            SeekFrom::Start(n) => self.read_pos = n as usize,
            SeekFrom::End(n) => unimplemented!(),
            SeekFrom::Current(n) => self.read_pos += n as usize,
        }
        Ok(self.read_pos as u64)
    }
}

pub struct Entry {
    entry: *mut ffi::Struct_archive_entry,
}

impl Entry {
    fn new(entry: *mut ffi::Struct_archive_entry) -> Entry {
        Entry { entry: entry }
    }

    pub fn pathname(&self) -> PathBuf {
        let c_str = unsafe { CStr::from_ptr(ffi::archive_entry_pathname(self.entry)) };
        PathBuf::from(c_str.to_string_lossy().as_ref())
    }

    pub fn size(&self) -> libc::int64_t {
        unsafe { ffi::archive_entry_size(self.entry) }
    }

    pub fn filetype(&self) -> libc::mode_t {
        unsafe { ffi::archive_entry_filetype(self.entry) }
    }
}

pub struct RefEntry<'a, R: SeekableRead + 'a> {
    e: Entry,
    _m: marker::PhantomData<&'a R>,
}

impl<'a, R: SeekableRead> RefEntry<'a, R> {
    fn new(e: Entry) -> RefEntry<'a, R> {
        RefEntry {
            e: e,
            _m: marker::PhantomData,
        }
    }

    pub fn pathname(&self) -> PathBuf {
        self.e.pathname()
    }

    pub fn size(&self) -> libc::int64_t {
        self.e.size()
    }

    pub fn filetype(&self) -> libc::mode_t {
        self.e.filetype()
    }
}
