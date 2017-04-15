extern crate env_logger;
extern crate fuse;
#[macro_use]
extern crate log;

use std::iter::FromIterator;
use std::vec::Vec;

mod fs;
mod archive;
mod physical;

fn main() {
    env_logger::init().unwrap();
    let args = Vec::<String>::from_iter(std::env::args());
    let ref target = args[1];
    let ref mountpoint = args[2];
    let mut fs = fs::ShowFS::new(target);
    let max_cache = 1024 * 1024 * 1024;
    fs.register_viewer(archive::ArchiveViewer::new(max_cache).unwrap());
    let result = fs.mount(mountpoint);
    result.unwrap();
}
