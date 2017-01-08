extern crate env_logger;
extern crate fuse;
#[macro_use]
extern crate log;

use std::iter::FromIterator;
use std::vec::Vec;

mod archive;
mod buffer;
mod fs;
mod physical;

fn main() {
    env_logger::init().unwrap();
    let args = Vec::<String>::from_iter(std::env::args());
    let ref target = args[1];
    let ref mountpoint = args[2];
    let mut fs = fs::ShowFS::new(target);
    fs.register_viewer(archive::view_archive);
    let result = fs.mount(mountpoint);
    result.unwrap();
}
