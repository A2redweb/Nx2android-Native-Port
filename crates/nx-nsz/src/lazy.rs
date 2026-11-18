//! The NSP an NSZ / XCZ stands for, read without writing it anywhere: `Read + Seek` over the decompressed dump, each NCA decompressed and encrypted again only where it is read.
//! A game's NCAs are tens of GB once decompressed; the extraction reads each of them mostly forward, so a block-compressed NCZ is read by blocks, and a solid one is decoded
//! forward on a thread, restarted from its start only when something reads behind the point it has reached.
//! The name check `decompress` makes (the SHA-256 of each NCA) cannot be made on pieces; the NCA headers' own hash tables are what `nxn verify` checks.

use super::*;
use crate::posfile::PosFile as File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver};

struct Ncz {
    /// the NCA's first 0x4000 bytes, as the NCZ keeps them
    head: Vec<u8>,
    segs: Vec<Seg>,
    total: u64,
    /// block mode: (block size, the dump offset of each block, its compressed size, the stream's size)
    block: Option<(u64, Vec<u64>, Vec<u32>, u64)>,
    /// solid mode: where the zstd stream starts in the dump, and its length
    solid: (u64, u64),
}

struct Ent {
    start: u64,
    size: u64,
    src: u64,
    ncz: Option<Ncz>,
}

/// A zstd stream decoded on a thread, handed over in chunks
struct Solid {
    rx: Receiver<io::Result<Vec<u8>>>,
    chunk: Vec<u8>,
    at: usize,
    /// the stream offset of chunk[at]
    pos: u64,
    done: bool,
}

struct ChanWriter {
    tx: std::sync::mpsc::SyncSender<io::Result<Vec<u8>>>,
    buf: Vec<u8>,
}
impl Write for ChanWriter {
    fn write(&mut self, d: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(d);
        if self.buf.len() >= 1 << 20 {
            self.flush()?;
        }
        Ok(d.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            self.tx.send(Ok(std::mem::take(&mut self.buf))).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "reader went away"))?;
        }
        Ok(())
    }
}

impl Solid {
    fn start(path: &Path, at: u64, len: u64, name: String) -> io::Result<Solid> {
        let mut f = File::open(path)?;
        f.seek(SeekFrom::Start(at))?;
        let (tx, rx) = sync_channel(4);
        std::thread::spawn(move || {
            let mut w = ChanWriter { tx: tx.clone(), buf: vec![] };
            let mut input = BufReader::with_capacity(1 << 20, f.take(len));
            match Decoder::default().decode(&mut input, &mut w).and_then(|_| w.flush()) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {}
                Err(e) => {
                    let _ = tx.send(Err(io::Error::new(e.kind(), format!("{name} does not decompress ({e}): a damaged dump"))));
                }
            }
        });
        Ok(Solid { rx, chunk: vec![], at: 0, pos: 0, done: false })
    }

    /// Fills `out` with the stream from `off` on (as far as the stream goes); the stream must not have passed `off`
    fn read(&mut self, off: u64, out: &mut [u8]) -> io::Result<usize> {
        let mut got = 0;
        loop {
            if self.at < self.chunk.len() {
                let skip = off.saturating_add(got as u64).saturating_sub(self.pos) as usize;
                let avail = self.chunk.len() - self.at;
                if skip > 0 {
                    let k = skip.min(avail);
                    self.at += k;
                    self.pos += k as u64;
                    continue;
                }
                let k = avail.min(out.len() - got);
                out[got..got + k].copy_from_slice(&self.chunk[self.at..self.at + k]);
                self.at += k;
                self.pos += k as u64;
                got += k;
                if got == out.len() {
                    return Ok(got);
                }
                continue;
            }
            if self.done {
                return Ok(got);
            }
            match self.rx.recv() {
                Ok(Ok(c)) => {
                    self.chunk = c;
                    self.at = 0;
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => self.done = true,
            }
        }
    }
}

pub struct LazyNsp {
    path: PathBuf,
    file: File,
    header: Vec<u8>,
    ents: Vec<Ent>,
    size: u64,
    pos: u64,
    name: String,
    /// the NCZ the solid decoder belongs to, and the decoder
    solid: Option<(usize, Solid)>,
    /// the last block read: (entry, block index, its bytes)
    block: Option<(usize, u64, Vec<u8>)>,
}

impl LazyNsp {
    pub fn open(path: &Path, name: &str) -> io::Result<LazyNsp> {
        let mut file = File::open(path)?;
        let plan = plan(&mut file)?;
        let names: Vec<String> = plan.iter().map(|e| e.0.clone()).collect();
        let sizes: Vec<u64> = plan.iter().map(|e| e.3).collect();
        let header = pfs0_header(&names, &sizes);
        let mut ents = vec![];
        let mut start = header.len() as u64;
        for (n, off, len, size, is_ncz) in &plan {
            let ncz = if *is_ncz {
                file.seek(SeekFrom::Start(*off))?;
                let head_raw = rd(&mut file, HEAD as usize)?;
                let h = head(&mut file, *off, &format!("{name}: {n}"))?;
                let (segs, total) = make_segs(&h, &format!("{name}: {n}"))?;
                let after = file.stream_position()?;
                let block = h.block.as_ref().map(|b| {
                    let mut at = after;
                    let offs: Vec<u64> = b.sizes.iter().map(|&s| {
                        let o = at;
                        at += s as u64;
                        o
                    }).collect();
                    (b.bs, offs, b.sizes.clone(), b.size)
                });
                Some(Ncz { head: head_raw, segs, total, block, solid: (after, (off + len).saturating_sub(after)) })
            } else {
                None
            };
            ents.push(Ent { start, size: *size, src: *off, ncz });
            start += size;
        }
        Ok(LazyNsp { path: path.to_path_buf(), file, header, ents, size: start, pos: 0, name: name.to_string(), solid: None, block: None })
    }

    pub fn len(&self) -> u64 {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// The decompressed (not yet encrypted again) stream of NCZ `ei` from stream offset `so`
    fn stream(&mut self, ei: usize, so: u64, out: &mut [u8]) -> io::Result<usize> {
        let ncz = self.ents[ei].ncz.as_ref().unwrap();
        if let Some((bs, offs, sizes, total)) = &ncz.block {
            let (bs, total) = (*bs, *total);
            let (bi, within) = (so / bs, (so % bs) as usize);
            if bi as usize >= offs.len() {
                return Ok(0);
            }
            let cached = matches!(&self.block, Some((e, b, _)) if *e == ei && *b == bi);
            if !cached {
                let (cs, at) = (sizes[bi as usize] as usize, offs[bi as usize]);
                let last = bi as usize + 1 == sizes.len();
                let want = if !last { bs } else { total - bs * bi };
                self.file.seek(SeekFrom::Start(at))?;
                let c = rd(&mut self.file, cs)?;
                let data = if (cs as u64) < want {
                    let mut o = Vec::with_capacity(want as usize);
                    nx_zstd::decompress(&mut &c[..], &mut o).map_err(|e| io::Error::new(e.kind(), format!("{} block {bi} does not decompress ({e}): a damaged dump", self.name)))?;
                    o
                } else {
                    c
                };
                self.block = Some((ei, bi, data));
            }
            let d = &self.block.as_ref().unwrap().2;
            let k = out.len().min(d.len().saturating_sub(within));
            out[..k].copy_from_slice(&d[within..within + k]);
            return Ok(k);
        }
        let (at, len) = ncz.solid;
        let restart = match &self.solid {
            Some((e, s)) => *e != ei || s.pos > so,
            None => true,
        };
        if restart {
            self.solid = Some((ei, Solid::start(&self.path, at, len, self.name.clone())?));
        }
        self.solid.as_mut().unwrap().1.read(so, out)
    }

    fn read_at(&mut self, pos: u64, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || pos >= self.size {
            return Ok(0);
        }
        if (pos as usize) < self.header.len() {
            let k = buf.len().min(self.header.len() - pos as usize);
            buf[..k].copy_from_slice(&self.header[pos as usize..pos as usize + k]);
            return Ok(k);
        }
        let ei = self.ents.partition_point(|e| e.start + e.size <= pos);
        let Some(e) = self.ents.get(ei) else { return Ok(0) };
        let o = pos - e.start;
        let k = (buf.len() as u64).min(e.size - o) as usize;
        let Some(ncz) = &e.ncz else {
            let src = e.src;
            self.file.seek(SeekFrom::Start(src + o))?;
            return self.file.read(&mut buf[..k]);
        };
        if o < HEAD {
            let k = k.min((HEAD - o) as usize);
            buf[..k].copy_from_slice(&ncz.head[o as usize..o as usize + k]);
            return Ok(k);
        }
        // a stream is read, then encrypted again by the section the bytes lie in
        let seg = {
            let i = ncz.segs.partition_point(|s| s.1 <= o);
            ncz.segs.get(i).map(|s| (s.1, s.2.clone(), s.3))
        };
        let (seg_end, aes, ctr8) = seg.unwrap_or((ncz.total, None, [0; 8]));
        let k = k.min((seg_end - o) as usize).min(1 << 20);
        let got = self.stream(ei, o - HEAD, &mut buf[..k])?;
        if got == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("{}: an NCZ ends before its NCA does (a damaged or cut-short dump)", self.name)));
        }
        if let Some(aes) = aes {
            let skew = (o & 15) as usize;
            let mut tmp = vec![0u8; skew + got];
            tmp[skew..].copy_from_slice(&buf[..got]);
            let mut ctr = [0u8; 16];
            ctr[..8].copy_from_slice(&ctr8);
            ctr[8..].copy_from_slice(&((o - skew as u64) >> 4).to_be_bytes());
            aes.ctr_apply(ctr, &mut tmp);
            buf[..got].copy_from_slice(&tmp[skew..]);
        }
        Ok(got)
    }
}

impl Read for LazyNsp {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.read_at(self.pos, buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for LazyNsp {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let p = match to {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => self.size as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if p < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"));
        }
        self.pos = p as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests as t;
    use super::*;

    #[test]
    fn the_lazy_view_is_the_eager_nsp_byte_for_byte() {
        for (name, dump) in t::sample_dumps() {
            let dir = std::env::temp_dir().join(format!("nxlazy-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join("a.nsz");
            std::fs::write(&p, &dump).unwrap();
            let mut eager = vec![];
            decompress(&mut io::Cursor::new(&dump), &mut eager, "x", &mut |_| {}).unwrap();
            let mut lazy = LazyNsp::open(&p, "x").unwrap();
            assert_eq!(lazy.len(), eager.len() as u64, "{name}");
            let mut all = vec![];
            lazy.read_to_end(&mut all).unwrap();
            assert!(all == eager, "{name}: whole read differs");
            // reads at odd places, backwards and forwards, odd lengths
            let mut x = 12345u64;
            for _ in 0..200 {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let off = (x >> 33) % eager.len() as u64;
                let len = ((x >> 20) % 3000 + 1) as usize;
                lazy.seek(SeekFrom::Start(off)).unwrap();
                let mut b = vec![0u8; len];
                let mut got = 0;
                while got < len {
                    let n = lazy.read(&mut b[got..]).unwrap();
                    if n == 0 {
                        break;
                    }
                    got += n;
                }
                let want = &eager[off as usize..(off as usize + len).min(eager.len())];
                assert_eq!(&b[..got], want, "{name} at {off}");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
