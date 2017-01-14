extern crate libc;

use std::io::{Read, Seek, SeekFrom, Result, Error};
use std::cmp::min;

const PAGE_SIZE: usize = 4096;

pub struct BufferedReader<R: Read> {
    r: R,
    read_pos: usize,
    size: usize,
    data: Vec<u8>,
}

impl<R: Read> BufferedReader<R> {
    pub fn new(r: R) -> BufferedReader<R> {
        BufferedReader {
            r: r,
            read_pos: 0,
            size: 0,
            data: Vec::new(),
        }
    }
}

impl<R: Read> Seek for BufferedReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        match pos {
            SeekFrom::Start(n) => {
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

fn round_to_page_size(n: usize) -> usize {
    let mut n_page = n / PAGE_SIZE;
    let remain = n % PAGE_SIZE;
    if remain > 0 {
        n_page += 1;
    }
    return n_page * PAGE_SIZE;
}

impl<R: Read> Read for BufferedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.read_pos >= self.size {
            let want = round_to_page_size(self.read_pos + buf.len() - self.size);
            let mut read = 0;
            self.data.resize(self.size + want, 0);
            while read < want {
                match self.r.read(&mut self.data[self.size..]) {
                    Ok(n) if n == 0 => break,
                    Ok(n) => {
                        read += n;
                        self.size += n;
                    }
                    e @ Err(_) => return e,
                }
            }
        }
        let l = min(self.size - self.read_pos, buf.len());
        buf[..l].copy_from_slice(&self.data[self.read_pos..self.read_pos + l]);
        self.read_pos += l;
        Ok(l)
    }
}

#[test]
fn test_read() {
    let mut v = Vec::<u8>::new();
    v.resize(2 * 1024 * 1024, 0);
    for e in v.iter_mut() {
        *e = unsafe { libc::rand() } as u8;
    }

    let mut r = BufferedReader::new(&v[..]);
    let mut out = Vec::<u8>::new();
    assert_eq!(r.read_to_end(&mut out).unwrap(), 2 * 1024 * 1024);

    assert_eq!(v, out);
}
