//! A minimal tar archive builder using only std::io.

use std::io::{Result, Write};

#[repr(C)]
#[derive(Clone, Copy)]
struct Header {
    name: [u8; 100],
    mode: [u8; 8],
    uid: [u8; 8],
    gid: [u8; 8],
    size: [u8; 12],
    mtime: [u8; 12],
    checksum: [u8; 8],
    type_flag: u8,
    linkname: [u8; 100],
    magic: [u8; 6],
    version: [u8; 2],
    uname: [u8; 32],
    gname: [u8; 32],
    devmajor: [u8; 8],
    devminor: [u8; 8],
    prefix: [u8; 155],
    padding: [u8; 12],
}

impl Header {
    fn new(size: u64, name: &str, prefix: &str) -> Self {
        let full_name = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", prefix, name)
        };
        let name_bytes = full_name.as_bytes();
        let mut header = Self::default();
        header.name[..name_bytes.len()].copy_from_slice(name_bytes);
        header.size = octal_bytes(size);
        header.type_flag = b'0';
        header.magic = *b"ustar\0";
        header.version = *b"00";
        header
    }

    fn checksum(&self) -> u32 {
        let mut raw = *self;
        raw.checksum = [0u8; 8];
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &raw as *const Self as *const u8,
                std::mem::size_of::<Self>(),
            )
        };
        bytes.iter().map(|&b| b as u32).sum()
    }

    fn as_bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self as *const Self as *const u8,
                std::mem::size_of::<Self>(),
            )
        }
    }
}

impl Default for Header {
    fn default() -> Self {
        Self {
            name: [0u8; 100],
            mode: [0u8; 8],
            uid: [0u8; 8],
            gid: [0u8; 8],
            size: [0u8; 12],
            mtime: [0u8; 12],
            checksum: [0u8; 8],
            type_flag: 0,
            linkname: [0u8; 100],
            magic: [0u8; 6],
            version: [0u8; 2],
            uname: [0u8; 32],
            gname: [0u8; 32],
            devmajor: [0u8; 8],
            devminor: [0u8; 8],
            prefix: [0u8; 155],
            padding: [0u8; 12],
        }
    }
}

fn octal_bytes(mut value: u64) -> [u8; 12] {
    let mut bytes = [b'0'; 12];
    for i in (0..12).rev() {
        bytes[i] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    bytes
}

pub struct Builder<W: Write> {
    inner: W,
    prefix: String,
}

impl<W: Write> Builder<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            prefix: String::new(),
        }
    }

    pub fn set_prefix(&mut self, prefix: &str) {
        self.prefix = prefix.to_string();
    }

    pub fn append_file(&mut self, name: &str, data: &[u8]) -> Result<()> {
        let size = data.len() as u64;
        let mut header = Header::new(size, name, &self.prefix);
        let checksum = header.checksum();
        let checksum_bytes = octal_bytes(checksum as u64);
        header.checksum[..checksum_bytes.len()].copy_from_slice(&checksum_bytes);

        self.inner.write_all(header.as_bytes())?;
        self.inner.write_all(data)?;
        // Pad to 512-byte blocks
        let padded = (512 - (data.len() % 512)) % 512;
        self.inner.write_all(&vec![0u8; padded])?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        // Write two empty 512-byte blocks to mark end of archive
        self.inner.write_all(&[0u8; 1024])?;
        Ok(())
    }
}
