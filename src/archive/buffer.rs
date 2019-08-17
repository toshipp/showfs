extern crate memmap;
extern crate tempfile;

use std::io::Result;
use std::ptr;

pub struct Buffer {
    inner: memmap::MmapMut,
    raw: *mut u8,
}

impl Buffer {
    pub fn new(size: usize) -> Result<Buffer> {
        let file = tempfile::tempfile()?;
        file.set_len(size as u64)?;
        unsafe {
            let inner = memmap::MmapMut::map_mut(&file)?;
            let mut b = Buffer {
                inner: inner,
                raw: ptr::null_mut(),
            };
            b.raw = b.inner.as_mut().as_mut_ptr();
            Ok(b)
        }
    }

    pub unsafe fn ptr(&self) -> *mut u8 {
        self.raw
    }
}

#[test]
fn test_buffer() {
    use std::slice;
    let b = Buffer::new(1).unwrap();
    let mut s = unsafe { slice::from_raw_parts_mut(b.ptr(), 1) };
    s[0] = 0x10;
    assert_eq!(s[0], 0x10);
}
