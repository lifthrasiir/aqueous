//! A system the sandbox does not lock down workers on: no worker starts.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::Path;

fn unsupported() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "workers are not locked down on this system")
}

pub enum Child {}

impl Child {
    pub fn limit_memory(&self, _: u64) -> io::Result<()> {
        match *self {}
    }

    pub fn share(&mut self, _: &Arena) -> io::Result<u64> {
        match *self {}
    }

    pub fn exit_code(&mut self) -> Option<i64> {
        match *self {}
    }
}

pub fn spawn(_: &Path, _: &[OsString]) -> io::Result<(Child, File, File)> {
    Err(unsupported())
}

pub enum Mapping {}

impl Mapping {
    pub fn as_ptr(&self) -> *mut u8 {
        match *self {}
    }

    pub fn len(&self) -> usize {
        match *self {}
    }
}

pub struct Arena {
    pub map: Mapping,
}

pub fn arena(_: usize) -> io::Result<Arena> {
    Err(unsupported())
}

pub fn view(_: u64, _: usize) -> io::Result<Mapping> {
    Err(unsupported())
}

pub fn lockdown() -> io::Result<()> {
    Err(unsupported())
}
