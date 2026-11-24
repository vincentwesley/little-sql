//! Byte-level storage behind the pager.
//!
//! The engine never touches the file system directly; it reads and writes
//! through this trait so the same code runs on a file natively and on a
//! byte buffer in WebAssembly and in tests.

use crate::error::Result;
use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub trait Storage {
    /// Current length in bytes.
    fn len(&self) -> Result<u64>;
    /// Read up to `buf.len()` bytes at `offset`; returns how many were read
    /// (fewer only at end of storage).
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize>;
    /// Write `data` at `offset`, extending the storage if needed.
    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<()>;
    fn truncate(&mut self, len: u64) -> Result<()>;
    /// Flush to durable storage.
    fn sync(&mut self) -> Result<()>;
    fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// A file on disk. A "lazy" file is not created until the first write, so
/// opening a database does not leave an empty `-wal` file behind.
pub struct FileStorage {
    file: Option<File>,
    path: PathBuf,
}

impl FileStorage {
    pub fn open_readonly(path: &Path) -> Result<FileStorage> {
        Ok(FileStorage {
            file: Some(File::open(path)?),
            path: path.to_path_buf(),
        })
    }

    /// Open for reading and writing, creating the file if it is missing.
    pub fn open_readwrite(path: &Path) -> Result<FileStorage> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        Ok(FileStorage {
            file: Some(file),
            path: path.to_path_buf(),
        })
    }

    /// Open for reading and writing if the file exists; otherwise create it
    /// on the first write.
    pub fn lazy(path: &Path) -> FileStorage {
        let file = OpenOptions::new().read(true).write(true).open(path).ok();
        FileStorage {
            file,
            path: path.to_path_buf(),
        }
    }

    fn file_mut(&mut self) -> Result<&mut File> {
        if self.file.is_none() {
            self.file = Some(
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(&self.path)?,
            );
        }
        Ok(self.file.as_mut().unwrap())
    }
}

impl Storage for FileStorage {
    fn len(&self) -> Result<u64> {
        match &self.file {
            Some(f) => Ok(f.metadata()?.len()),
            None => Ok(0),
        }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let Some(mut f) = self.file.as_ref() else {
            return Ok(0);
        };
        f.seek(SeekFrom::Start(offset))?;
        let mut done = 0;
        while done < buf.len() {
            let n = f.read(&mut buf[done..])?;
            if n == 0 {
                break;
            }
            done += n;
        }
        Ok(done)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let f = self.file_mut()?;
        f.seek(SeekFrom::Start(offset))?;
        f.write_all(data)?;
        Ok(())
    }

    fn truncate(&mut self, len: u64) -> Result<()> {
        if self.file.is_some() || len > 0 {
            self.file_mut()?.set_len(len)?;
        }
        Ok(())
    }

    fn sync(&mut self) -> Result<()> {
        if let Some(f) = &self.file {
            f.sync_data()?;
        }
        Ok(())
    }
}

/// A growable in-memory buffer. Clones share the same bytes, so a caller can
/// keep a handle and read the database image back after the engine wrote it.
#[derive(Clone, Default)]
pub struct MemStorage {
    data: Rc<RefCell<Vec<u8>>>,
}

impl MemStorage {
    pub fn new(bytes: Vec<u8>) -> MemStorage {
        MemStorage {
            data: Rc::new(RefCell::new(bytes)),
        }
    }

    /// A copy of the current contents.
    pub fn bytes(&self) -> Vec<u8> {
        self.data.borrow().clone()
    }
}

impl Storage for MemStorage {
    fn len(&self) -> Result<u64> {
        Ok(self.data.borrow().len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let data = self.data.borrow();
        let start = (offset as usize).min(data.len());
        let n = buf.len().min(data.len() - start);
        buf[..n].copy_from_slice(&data[start..start + n]);
        Ok(n)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        let mut data = self.data.borrow_mut();
        let end = offset as usize + bytes.len();
        if data.len() < end {
            data.resize(end, 0);
        }
        data[offset as usize..end].copy_from_slice(bytes);
        Ok(())
    }

    fn truncate(&mut self, len: u64) -> Result<()> {
        self.data.borrow_mut().truncate(len as usize);
        Ok(())
    }

    fn sync(&mut self) -> Result<()> {
        Ok(())
    }
}
