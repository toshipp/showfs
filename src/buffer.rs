extern crate libc;

use std::io::{Read, Seek, SeekFrom, Result, Error};
use std::cmp::min;

pub struct BufferedReader<R: Read> {
    r: R,
    read_pos: usize,
    data: Vec<u8>,
    rbuf: Vec<u8>,
}

impl<R: Read> BufferedReader<R> {
    pub fn new(r: R) -> BufferedReader<R> {
        let mut rbuf = Vec::new();
        rbuf.resize(1024 * 1024, 0);
        BufferedReader {
            r: r,
            read_pos: 0,
            data: Vec::new(),
            rbuf: rbuf,
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

impl<R: Read> Read for BufferedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.read_pos >= self.data.len() {
            let size = self.read_pos + buf.len() - self.data.len();
            let mut read = 0;
            while read < size {
                match self.r.read(&mut self.rbuf) {
                    Ok(n) if n == 0 => break,
                    Ok(n) => {
                        self.data.extend_from_slice(&self.rbuf[..n]);
                        read += n;
                    }
                    e @ Err(_) => return e,
                }
            }
        }
        let l = min(self.data.len() - self.read_pos, buf.len());
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
