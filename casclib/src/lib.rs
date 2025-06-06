extern crate casclib_sys as casclib;

use std::error;
use std::ffi::{CStr, CString};
use std::fmt;
use std::io::{self, SeekFrom};
use std::mem;
use std::os::raw::c_char;
use std::path::Path;
use std::ptr;

use casclib::HANDLE;

#[derive(Debug)]
pub enum CascError {
    Io(io::Error),
    InvalidPath,
    InvalidFileName,
    FileNotFound,
    #[cfg(not(target_os = "windows"))]
    NonUtf8,
    Code(casclib::ErrorCode),
}

impl CascError {
    unsafe fn from_last_error() -> Self {
        match casclib::GetCascError() {
            casclib_sys::ERROR_FILE_NOT_FOUND => CascError::FileNotFound,
            v => CascError::Code(v),
        }
    }
}

impl fmt::Display for CascError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            CascError::Io(ref err) => err.fmt(f),
            CascError::InvalidPath => write!(f, "Invalid storage path"),
            CascError::InvalidFileName => write!(f, "Invalid file name"),
            CascError::FileNotFound => write!(f, "File not found"),
            #[cfg(not(target_os = "windows"))]
            CascError::NonUtf8 => write!(f, "Non-utf-8 encoding is not supported"),
            CascError::Code(code) => write!(f, "Error code: {}", code),
        }
    }
}

impl error::Error for CascError {
    fn cause(&self) -> Option<&dyn error::Error> {
        match *self {
            CascError::Io(ref err) => Some(err),
            _ => None,
        }
    }
}

pub fn open<P: AsRef<Path>>(path: P) -> Result<Storage, CascError> {
    #[cfg(not(target_os = "windows"))]
    let cpath = {
        let pathstr = path.as_ref().to_str().ok_or(CascError::NonUtf8)?;
        CString::new(pathstr).map_err(|_| CascError::InvalidPath)?
    };
    #[cfg(target_os = "windows")]
    let cpath = {
        use widestring::U16CString;
        U16CString::from_os_str(path.as_ref())
            .map_err(|_| CascError::InvalidPath)?
            .into_vec()
    };
    let handle = unsafe {
        let mut handle: HANDLE = ptr::null_mut();
        let ok = casclib::CascOpenStorage(cpath.as_ptr(), 0, &mut handle as *mut HANDLE);
        if !ok {
            return Err(CascError::from_last_error());
        }
        handle
    };

    Ok(Storage {
        handle,
        file_count: Storage::read_file_count(handle)?,
    })
}

#[derive(Debug)]
pub struct Storage {
    handle: HANDLE,
    file_count: u32,
}

// https://github.com/ladislav-zezula/CascLib/issues/172#issuecomment-556076211
unsafe impl Send for Storage {}

unsafe impl Sync for Storage {}

impl Drop for Storage {
    fn drop(&mut self) {
        unsafe {
            casclib::CascCloseStorage(self.handle);
        }
    }
}

impl Storage {
    fn read_file_count(handle: HANDLE) -> Result<u32, CascError> {
        unsafe {
            let mut count: u32 = 0;
            let ok = casclib::CascGetStorageInfo(
                handle,
                casclib::_CASC_STORAGE_INFO_CLASS_CascStorageTotalFileCount,
                mem::transmute::<*mut u32, *mut std::ffi::c_void>(&mut count as *mut u32),
                4,
                ptr::null_mut(),
            );
            if !ok {
                return Err(CascError::from_last_error());
            }
            Ok(count)
        }
    }

    pub fn file_count(&self) -> u32 {
        self.file_count
    }

    pub fn files<T>(&self) -> Find
    where
        T: AsRef<[u8]>,
    {
        self.files_with_mask("*")
    }

    pub fn files_with_mask<T>(&self, mask: T) -> Find
    where
        T: AsRef<[u8]>,
    {
        Find {
            mask: CString::new(mask.as_ref())
                .ok()
                .unwrap_or_else(|| CString::new("*").unwrap()),
            storage: self,
        }
    }

    pub fn entry<T>(&self, name: T) -> FileEntry
    where
        T: Into<String>,
    {
        FileEntry {
            storage: self,
            name: name.into(),
        }
    }

    pub fn enumerate_files(
        &self,
        pattern: &str,
        on_file_found: &mut dyn FnMut(&str, usize),
    ) -> Result<usize, CascError> {
        let mut entries_consumed = 0;

        unsafe {
            let mut data: casclib::CASC_FIND_DATA = std::mem::zeroed(); // Safer alternative
            let c_pattern = CString::new(pattern).unwrap_or_else(|_| CString::new("*").unwrap());
            let file_handle = casclib::CascFindFirstFile(
                self.handle,
                c_pattern.as_ptr(),
                &mut data as *mut casclib::CASC_FIND_DATA,
                ptr::null(),
            );
            if file_handle.is_null() {
                let code = casclib::GetCascError();
                if code == casclib::ERROR_NO_MORE_FILES {
                    return Ok(entries_consumed);
                } else {
                    return Err(CascError::Code(code));
                }
            } else {
                loop {
                    if data.bFileAvailable() != 0 {
                        let file_name = CStr::from_ptr(&data.szFileName as *const c_char)
                            .to_str()
                            .map_err(|_| CascError::InvalidFileName)?;
                        on_file_found(file_name, data.FileSize as usize);
                        entries_consumed += 1;
                    }

                    let ok = casclib::CascFindNextFile(
                        file_handle,
                        &mut data as *mut casclib::CASC_FIND_DATA,
                    );
                    if !ok {
                        break;
                    }
                }
            }
            casclib::CascFindClose(file_handle);
        }
        Ok(entries_consumed)
    }
}

#[derive(Debug)]
pub struct Find<'a> {
    mask: CString,
    storage: &'a Storage,
}

impl<'a> IntoIterator for Find<'a> {
    type Item = Result<FileEntry<'a>, CascError>;
    type IntoIter = FindIterator<'a>;

    fn into_iter(self) -> Self::IntoIter {
        FindIterator {
            find: self,
            handle: None,
            data: unsafe { std::mem::MaybeUninit::zeroed().assume_init() },
        }
    }
}

pub struct FindIterator<'a> {
    find: Find<'a>,
    handle: Option<HANDLE>,
    data: casclib::CASC_FIND_DATA,
}

impl fmt::Debug for FindIterator<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FindIterator")
            .field("find", &self.find)
            .field("handle", &self.handle)
            .finish()
    }
}

impl<'a> FindIterator<'a> {
    fn find_first<'b>(&'b mut self) -> Option<Result<FileEntry<'a>, CascError>> {
        let mask = self.find.mask.as_ptr();
        unsafe {
            let handle = casclib::CascFindFirstFile(
                self.find.storage.handle,
                mask,
                &mut self.data as *mut casclib::CASC_FIND_DATA,
                ptr::null(),
            );
            if handle.is_null() {
                let code = casclib::GetCascError();
                if code == casclib::ERROR_NO_MORE_FILES {
                    return None;
                } else {
                    return Err(CascError::Code(code)).into();
                }
            }
            self.handle = Some(handle)
        }
        Some(self.get_entry())
    }

    fn get_entry<'b>(&'b self) -> Result<FileEntry<'a>, CascError> {
        unsafe {
            let s = CStr::from_ptr(&self.data.szFileName as *const c_char)
                .to_str()
                .map_err(|_| CascError::InvalidFileName)?;
            Ok(FileEntry::new(self.find.storage, s))
        }
    }
}

impl<'a> Iterator for FindIterator<'a> {
    type Item = Result<FileEntry<'a>, CascError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.handle {
            None => self.find_first(),
            Some(handle) => unsafe {
                let ok = casclib::CascFindNextFile(
                    handle,
                    &mut self.data as *mut casclib::CASC_FIND_DATA,
                );
                if ok {
                    Some(self.get_entry())
                } else {
                    None
                }
            },
        }
    }
}

#[derive(Debug)]
pub struct FileEntry<'a> {
    storage: &'a Storage,
    name: String,
}

impl<'a> FileEntry<'a> {
    fn new<'b>(storage: &'a Storage, name: &'b str) -> FileEntry<'a> {
        FileEntry {
            storage,
            name: name.to_string(),
        }
    }

    pub fn open(self) -> Result<File<'a>, CascError> {
        let mut file_handle: HANDLE = ptr::null_mut();
        unsafe {
            let name = CString::new(&self.name[..]).map_err(|_| CascError::InvalidFileName)?;
            let ok = casclib::CascOpenFile(
                self.storage.handle,
                std::mem::transmute::<*const i8, *const std::ffi::c_void>(name.as_ptr()),
                0,
                0,
                &mut file_handle as *mut HANDLE,
            );
            if !ok {
                return Err(CascError::from_last_error());
            }
        }

        let size: u64 = unsafe {
            let mut size_high: u32 = 0;
            let size_low: u32 = casclib::CascGetFileSize(file_handle, &mut size_high as *mut u32);
            let size: u64 = size_high as u64;
            (size << 32) | (size_low as u64)
        };

        Ok(File {
            entry: self,
            handle: file_handle,
            size,
        })
    }

    pub fn get_name(&self) -> &str {
        &self.name
    }
}

pub struct File<'a> {
    entry: FileEntry<'a>,
    handle: HANDLE,
    size: u64,
}

impl Drop for File<'_> {
    fn drop(&mut self) {
        unsafe {
            casclib::CascCloseFile(self.handle);
        }
    }
}

impl File<'_> {
    pub fn get_name(&self) -> &str {
        &self.entry.name
    }

    pub fn get_size(&self) -> u64 {
        self.size
    }

    pub fn read_all(&self) -> Result<Vec<u8>, CascError> {
        let size = self.get_size();
        let mut buf = Vec::with_capacity(size as usize);
        self.extract(&mut buf)?;
        Ok(buf)
    }

    pub fn read_struct<T>(&self) -> Result<T, CascError>
    where
        T: Copy, // Enforce T is trivially copyable
    {
        use std::{io, mem::MaybeUninit};
        let mut value = MaybeUninit::<T>::uninit();
        let size = std::mem::size_of::<T>() as u32;
        let mut bytes_read: u32 = 0;

        unsafe {
            let ok = casclib::CascReadFile(
                self.handle,
                value.as_mut_ptr() as *mut std::ffi::c_void,
                size,
                &mut bytes_read,
            );

            if !ok || bytes_read != size {
                return Err(CascError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "Failed to read struct: expected {} bytes, got {}",
                        size, bytes_read
                    ),
                )));
            }

            Ok(value.assume_init())
        }
    }

    pub fn read(&self, buf: &mut [u8]) -> Result<usize, CascError> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut bytes_read: u32 = 0;
        let ok = unsafe {
            casclib::CascReadFile(
                self.handle,
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                buf.len() as u32,
                &mut bytes_read as *mut u32,
            )
        };
        if ok {
            Ok(bytes_read as usize)
        } else {
            Err(CascError::Io(io::Error::new(
                io::ErrorKind::Other,
                format!("CascReadFile failed (code = {})", unsafe {
                    casclib::GetCascError()
                }),
            )))
        }
    }

    pub fn seek(&self, pos: SeekFrom) -> Result<u64, CascError> {
        let (offset, origin) = match pos {
            SeekFrom::Start(n) => (n as i64, 0), // FILE_BEGIN
            SeekFrom::End(n) => (n, 2),          // FILE_END
            SeekFrom::Current(n) => (n, 1),      // FILE_CURRENT
        };

        let mut result: u64 = 0;
        unsafe {
            let ok = casclib::CascSetFilePointer64(
                self.handle,
                offset,
                &mut result,
                origin as casclib::DWORD,
            );
            if ok {
                Ok(result)
            } else {
                Err(CascError::Code(casclib::GetCascError()))
            }
        }
    }

    pub fn extract<T: io::Write>(&self, mut w: T) -> Result<usize, CascError> {
        unsafe {
            casclib::SetCascError(0);
        }

        let mut buffer: [u8; 0x1000] = [0; 0x1000];
        unsafe {
            let pos = casclib::CascSetFilePointer(
                self.handle,
                0,
                std::ptr::null_mut::<casclib::LONG>(),
                casclib::FILE_BEGIN as casclib::DWORD,
            );
            if pos == casclib::CASC_INVALID_POS {
                return Err(CascError::Code(casclib::GetCascError()));
            }
        }

        unsafe {
            let mut bytes_write_total: usize = 0;
            let mut bytes_read: u32 = 1;
            while bytes_read != 0 {
                let ok = casclib::CascReadFile(
                    self.handle,
                    mem::transmute::<*mut u8, *mut std::ffi::c_void>(&mut buffer[0] as *mut u8),
                    mem::size_of_val(&buffer) as u32,
                    &mut bytes_read as *mut u32,
                );
                if !ok {
                    break;
                }
                let end_pos = bytes_read as usize;
                if bytes_read != 0 {
                    w.write_all(&buffer[0..end_pos]).map_err(CascError::Io)?;
                    bytes_write_total += end_pos;
                }
            }
            match casclib::GetCascError() {
                0 => Ok(bytes_write_total),
                code => Err(CascError::Io(io::Error::new(
                    io::ErrorKind::Other,
                    format!(
                        "CascReadFile error: code = {}, bytes_write_total = {}",
                        code, bytes_write_total
                    ),
                ))),
            }
        }
    }
}
