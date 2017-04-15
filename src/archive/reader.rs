extern crate libc;
use std::io::{Read, Seek, SeekFrom, Result, Error, ErrorKind};
use super::page::{WeakRefPage, RefPage, PageManager, SliceIter};
use std::cell::RefCell;
use fs::{File, SeekableRead};
use std::cmp::min;
use std::rc::Rc;


enum CacheState {
    Empty,
    Loading(Rc<RefCell<LoadingState<Box<SeekableRead>>>>),
    Loaded(WeakRefPage, usize),
}

pub struct Cache {
    page_manager: Rc<RefCell<PageManager>>,
    size: Option<usize>,
    file: Rc<File>,
    state: CacheState,
}

impl Cache {
    pub fn new(page_manager: Rc<RefCell<PageManager>>, file: Rc<File>) -> Cache {
        Cache {
            page_manager: page_manager,
            size: None,
            file: file,
            state: CacheState::Empty,
        }
    }

    pub fn make_reader(&mut self) -> Result<Box<SeekableRead>> {
        match self.state {
            CacheState::Empty => {
                if self.size.is_none() {
                    self.size = Some(self.file.getattr()?.size as usize);
                }
                let weak = self.page_manager
                    .borrow_mut()
                    .allocate(self.size.unwrap())
                    .ok_or(Error::new(ErrorKind::Other, "oom"))?;
                let page = weak.upgrade().unwrap();
                let reader = self.file.open()?;
                let loading_state = Rc::new(RefCell::new(LoadingState {
                    reader: Some(reader),
                    cached_size: 0,
                    page: page,
                }));
                self.state = CacheState::Loading(loading_state);
            }
            CacheState::Loading(_) => {
                let mut state = CacheState::Empty; // dummy
                if let CacheState::Loading(ref loading_state) = self.state {
                    if !loading_state.borrow().is_eof() {
                        return Ok(Box::new(LoadingReader {
                            size: self.size.unwrap(),
                            pos: 0,
                            state: loading_state.clone(),
                        }));
                    }
                    let cache_size = loading_state.borrow().cached_size;
                    let weak = loading_state.borrow().page.downgrade();
                    state = CacheState::Loaded(weak, cache_size)
                }
                self.state = state;
            }
            CacheState::Loaded(_, _) => {
                if let CacheState::Loaded(ref page, cache_size) = self.state {
                    if let Some(page) = page.upgrade() {
                        return Ok(Box::new(CacheReader {
                            size: cache_size,
                            pos: 0,
                            page: page,
                        }));
                    }
                }
                self.state = CacheState::Empty;
            }
        }
        self.make_reader()
    }
}

macro_rules! impl_seek {
    ($struct_: ident) => { impl_seek!{$struct_[ ]} };
    ($struct_: ident < $($v: ident : $trait_: ident),* >) => {
        impl_seek!{$struct_ [ $($v: $trait_)* ]}
    };
    ($struct_: ident [ $($v: ident : $trait_: ident),* ]) => {
        impl<$($v: $trait_)*> Seek for $struct_<$($v)*> {
            fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
                match pos {
                    SeekFrom::Start(n) => self.pos = n as usize,
                    SeekFrom::End(i) => {
                        if i < 0 && self.size < -i as usize {
                            return Err(Error::from_raw_os_error(libc::EINVAL));
                        } else {
                            self.pos = self.size + i as usize;
                        }
                    }
                    SeekFrom::Current(i) => {
                        if i < 0 && self.pos < -i as usize {
                            return Err(Error::from_raw_os_error(libc::EINVAL));
                        } else {
                            self.pos += i as usize;
                        }
                    }
                }
                Ok(self.pos as u64)
            }
        }
    };
}

struct CacheReader {
    size: usize,
    pos: usize,
    page: RefPage,
}

impl_seek!(CacheReader);

impl Read for CacheReader {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.pos >= self.size {
            return Ok(0);
        }
        let max = min(self.size - self.pos, buf.len());
        let mut read = 0;
        for slice in self.page.get_slices(self.pos) {
            if read >= max {
                break;
            }
            let l = min(slice.len(), max - read);
            &mut buf[read..read + l].copy_from_slice(&slice[..l]);
            read += l;
        }
        self.pos += read;
        Ok(read)
    }
}

struct LoadingState<R: Read> {
    reader: Option<R>,
    cached_size: usize,
    page: RefPage,
}

impl<R: Read> LoadingState<R> {
    fn get_slices(&self, pos: usize) -> SliceIter {
        self.page.get_slices(pos)
    }

    fn is_eof(&self) -> bool {
        self.reader.is_none()
    }

    fn read_to_at_least(&mut self, read_to: usize) -> Result<usize> {
        if self.is_eof() || self.cached_size >= read_to {
            return Ok(self.cached_size);
        }
        let mut iter = self.page.get_slices_mut(self.cached_size);
        while self.cached_size < read_to {
            let slice = match iter.next() {
                Some(slice) => slice,
                None => {
                    // no more buffer, close reader.
                    self.reader = None;
                    return Ok(self.cached_size);
                }
            };
            let mut n = 0;
            while n < slice.len() {
                let nn = self.reader.as_mut().unwrap().read(&mut slice[n..])?;
                if nn == 0 {
                    // reached eof, close reader.
                    self.reader = None;
                    return Ok(self.cached_size);
                }
                n += nn;
                self.cached_size += nn;
            }
        }
        Ok(self.cached_size)
    }
}

struct LoadingReader<R: Read> {
    size: usize,
    pos: usize,
    state: Rc<RefCell<LoadingState<R>>>,
}

impl_seek!(LoadingReader<R: Read>);

impl<R: Read> Read for LoadingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let cached_size = self.state.borrow_mut().read_to_at_least(self.pos + buf.len())?;
        if self.pos >= cached_size {
            return Ok(0);
        }
        let max = min(cached_size - self.pos, buf.len());
        let mut read = 0;
        for slice in self.state.borrow().get_slices(self.pos) {
            if read >= max {
                break;
            }
            let l = min(slice.len(), max - read);
            &mut buf[read..read + l].copy_from_slice(&slice[..l]);
            read += l;
        }
        self.pos += read;
        Ok(read)
    }
}

#[test]
fn test_read() {
    extern crate libc;
    use std::mem::zeroed;
    use fuse::FileAttr;
    use std::io::Cursor;
    use std::ffi::OsStr;
    struct VecFile {
        v: Vec<u8>,
        open_count: Rc<RefCell<u8>>,
    }
    impl File for VecFile {
        fn getattr(&self) -> Result<FileAttr> {
            let mut a = unsafe { zeroed::<FileAttr>() };
            a.size = self.v.len() as u64;
            Ok(a)
        }

        fn open(&self) -> Result<Box<SeekableRead>> {
            *self.open_count.borrow_mut() += 1;
            Ok(Box::new(Cursor::new(self.v.clone())))
        }

        fn name(&self) -> &OsStr {
            unimplemented!();
        }
    }

    let page_manager = Rc::new(RefCell::new(PageManager::new(10 * 1024 * 1024).unwrap()));
    let mut v = vec![0; 2 * 1024 * 1024];
    for e in v.iter_mut() {
        *e = unsafe { libc::rand() as u8 };
    }
    let open_count = Rc::new(RefCell::new(0));
    let file = Rc::new(VecFile {
        v: v.clone(),
        open_count: open_count.clone(),
    });
    let mut cache = Cache::new(page_manager.clone(), file);

    // first read.
    {
        let mut r = cache.make_reader().unwrap();
        let mut out = Vec::<u8>::new();
        assert_eq!(r.read_to_end(&mut out).unwrap(), 2 * 1024 * 1024);
        assert_eq!(v, out);
    }
    // second read.
    {
        let mut r = cache.make_reader().unwrap();
        let mut out = Vec::<u8>::new();
        assert_eq!(r.read_to_end(&mut out).unwrap(), 2 * 1024 * 1024);
        assert_eq!(v, out);
        assert_eq!(*open_count.borrow(), 1);
    }
}
