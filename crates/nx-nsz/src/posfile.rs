//! A file read by position. A dump chosen through Android's file picker arrives as an open descriptor, and the path `/proc/self/fd/N` that names it cannot be opened
//! again by the app (Permission denied: the storage layer checks the path, not the descriptor). So such a path is not reopened: its descriptor is duplicated, and
//! every reader reads at its own position (`pread`), as duplicated descriptors share one file offset.
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::Arc;

pub struct PosFile {
    file: Arc<File>,
    pos: u64,
}

impl PosFile {
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<PosFile> {
        let p = path.as_ref();
        let file = match p.to_str().and_then(|s| s.strip_prefix("/proc/self/fd/")).and_then(|n| n.parse::<i32>().ok()) {
            Some(fd) => {
                // SAFETY: the descriptor belongs to the caller, which keeps it open for the call; it is only duplicated here
                let owned: OwnedFd = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?;
                File::from(owned)
            }
            None => File::open(p)?,
        };
        Ok(PosFile { file: Arc::new(file), pos: 0 })
    }

    pub fn len(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }
}

impl Read for PosFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.file.read_at(buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for PosFile {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let p = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => self.len()?.checked_add_signed(d),
        };
        self.pos = p.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn a_descriptor_path_is_duplicated_and_each_reader_keeps_its_position() {
        let p = std::env::temp_dir().join(format!("nx2a-posfile-{}", std::process::id()));
        std::fs::write(&p, b"0123456789").unwrap();
        let orig = File::open(&p).unwrap();
        let path = format!("/proc/self/fd/{}", orig.as_raw_fd());
        let (mut a, mut b) = (PosFile::open(&path).unwrap(), PosFile::open(&path).unwrap());
        let mut x = [0u8; 3];
        a.read_exact(&mut x).unwrap();
        assert_eq!(&x, b"012");
        b.seek(SeekFrom::Start(7)).unwrap();
        b.read_exact(&mut x).unwrap();
        assert_eq!(&x, b"789");
        a.read_exact(&mut x).unwrap(); // a was not moved by b
        assert_eq!(&x, b"345");
        assert_eq!(a.seek(SeekFrom::End(-2)).unwrap(), 8);
        let _ = std::fs::remove_file(&p);
    }
}
