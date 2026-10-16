//! Finding what changed since the last update without scanning: the NTFS change journal.
//!
//! NTFS logs every create, delete, rename and write on the volume in its USN journal. The index
//! saves the journal position with each snapshot; the next update reads the records since then
//! and rescans only what they touch. Without elevation the journal is read through a directory
//! handle and its records carry file reference numbers (FRNs) but no names, so:
//!
//! - The index keeps the FRN of every indexed directory, with its parent's FRN and its name.
//!   Records from directories outside the tree miss this table and are ignored; renaming a
//!   directory changes one entry.
//! - A file record marks its directory, which is listed again (one enumeration covers every
//!   create, delete, rename and write in it).
//! - A directory record (create, rename, delete) rescans the subtree at its old path (from the
//!   table) and its new one (from the FRN, via `OpenFileById`).
//!
//! When the journal cannot answer (another file system, a different volume, a journal that was
//! recreated or has wrapped past the saved position) updates fall back to a full scan, which is
//! always correct.

/// Where the index stopped reading the journal. Saved in snapshots.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct JournalState {
    /// Volume serial number.
    pub(crate) volume: u64,
    pub(crate) id: u64,
    /// First USN not yet seen.
    pub(crate) next_usn: u64,
    /// FRN of the indexed root directory.
    pub(crate) root: u64,
    pub(crate) valid: bool,
}

impl JournalState {
    pub(crate) fn to_words(self) -> [u64; 5] {
        [
            self.volume,
            self.id,
            self.next_usn,
            self.root,
            self.valid as u64,
        ]
    }

    pub(crate) fn from_words(w: [u64; 5]) -> Self {
        Self {
            volume: w[0],
            id: w[1],
            next_usn: w[2],
            root: w[3],
            valid: w[4] != 0,
        }
    }
}

/// One journal record, already filtered to the fields the index needs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Record {
    pub(crate) frn: u64,
    pub(crate) parent: u64,
    pub(crate) reason: u32,
    pub(crate) directory: bool,
}

pub(crate) const REASON_CREATE: u32 = 0x100;
pub(crate) const REASON_DELETE: u32 = 0x200;
pub(crate) const REASON_RENAME_OLD: u32 = 0x1000;
pub(crate) const REASON_RENAME_NEW: u32 = 0x2000;

#[cfg(windows)]
pub(crate) use windows::Journal;

#[cfg(not(windows))]
pub(crate) use other::Journal;

#[cfg(not(windows))]
mod other {
    use std::io;
    use std::path::Path;

    use super::{JournalState, Record};

    /// No change journal: every update is a full scan. (Linux fanotify and macOS FSEvents come
    /// later.)
    pub(crate) struct Journal;

    impl Journal {
        pub(crate) fn open(_root: &Path) -> Option<Self> {
            None
        }

        pub(crate) fn root(&self) -> u64 {
            0
        }

        pub(crate) fn position(&self) -> io::Result<JournalState> {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }

        pub(crate) fn dir_frn(&self, _path: &Path) -> io::Result<u64> {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }

        pub(crate) fn relative_path(&self, _frn: u64) -> Option<String> {
            None
        }

        pub(crate) fn read(&self, _since: &JournalState) -> io::Result<Option<(Vec<Record>, u64)>> {
            Ok(None)
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_NAME_NORMALIZED,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdType,
        GetFileInformationByHandle, GetFinalPathNameByHandleW, GetVolumeInformationByHandleW,
        OPEN_EXISTING, OpenFileById, VOLUME_NAME_DOS,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_UNPRIVILEGED_USN_JOURNAL, FSCTL_READ_USN_JOURNAL,
        READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
    };

    use super::{JournalState, Record};

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: an open handle owned by this value.
            unsafe { CloseHandle(self.0) };
        }
    }

    // SAFETY: kernel handles may be used from any thread.
    unsafe impl Send for Handle {}
    // SAFETY: as above; DeviceIoControl on a volume handle is thread-safe.
    unsafe impl Sync for Handle {}

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain([0]).collect()
    }

    fn open(path: &[u16], access: u32) -> io::Result<Handle> {
        // SAFETY: NUL-terminated path; no security attributes or template.
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Handle(h))
    }

    /// The change journal of the volume holding a directory.
    pub(crate) struct Journal {
        /// The root directory. Unprivileged, journal control codes go to a handle of any
        /// directory on the volume; the volume device itself needs administrator rights.
        handle: Handle,
        serial: u64,
        root_frn: u64,
        /// The canonical root without its `\?` prefix, for turning paths relative.
        root: String,
    }

    impl Journal {
        pub(crate) fn open(root: &Path) -> Option<Self> {
            let root = std::fs::canonicalize(root).ok()?;
            let handle = open(&wide(&root), FILE_READ_ATTRIBUTES).ok()?;
            let mut serial = 0u32;
            let mut fs_name = [0u16; 32];
            // SAFETY: valid handle; unused outputs are null.
            let ok = unsafe {
                GetVolumeInformationByHandleW(
                    handle.0,
                    ptr::null_mut(),
                    0,
                    &mut serial,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    fs_name.as_mut_ptr(),
                    fs_name.len() as u32,
                )
            };
            let fs_len = fs_name.iter().position(|&c| c == 0).unwrap_or(0);
            // ReFS also has a journal, with 128-bit file ids: not supported yet.
            if ok == 0 || String::from_utf16_lossy(&fs_name[..fs_len]) != "NTFS" {
                return None;
            }
            let root_text = root.to_str()?;
            let mut journal = Self {
                handle,
                serial: u64::from(serial),
                root_frn: 0,
                root: root_text
                    .strip_prefix(r"\\?\")
                    .unwrap_or(root_text)
                    .to_owned(),
            };
            journal.root_frn = journal.dir_frn(&root).ok()?;
            journal.position().ok()?;
            Some(journal)
        }

        fn query(&self) -> io::Result<USN_JOURNAL_DATA_V0> {
            // SAFETY: all-zero is a valid USN_JOURNAL_DATA_V0.
            let mut data: USN_JOURNAL_DATA_V0 = unsafe { std::mem::zeroed() };
            let mut returned = 0;
            // SAFETY: valid volume handle; the output buffer is the struct the code fills.
            let ok = unsafe {
                DeviceIoControl(
                    self.handle.0,
                    FSCTL_QUERY_USN_JOURNAL,
                    ptr::null(),
                    0,
                    (&raw mut data).cast(),
                    size_of::<USN_JOURNAL_DATA_V0>() as u32,
                    &mut returned,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(data)
        }

        /// FRN of the indexed root directory.
        pub(crate) fn root(&self) -> u64 {
            self.root_frn
        }

        /// The journal's current end: everything before it has been seen once the scan that
        /// follows this call completes.
        pub(crate) fn position(&self) -> io::Result<JournalState> {
            let data = self.query()?;
            Ok(JournalState {
                volume: self.serial,
                id: data.UsnJournalID,
                next_usn: data.NextUsn as u64,
                root: self.root_frn,
                valid: true,
            })
        }

        /// The current path of FRN `frn` relative to the root (`""` for the root itself), or
        /// `None` when it no longer exists or lies outside the root. This is how records are
        /// resolved: the unprivileged journal API leaves names out of its records.
        pub(crate) fn relative_path(&self, frn: u64) -> Option<String> {
            let id = FILE_ID_DESCRIPTOR {
                dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
                Type: FileIdType,
                Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: frn as i64 },
            };
            // SAFETY: valid volume-relative handle and id descriptor; no security attributes.
            let h = unsafe {
                OpenFileById(
                    self.handle.0,
                    &id,
                    FILE_READ_ATTRIBUTES,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    ptr::null(),
                    FILE_FLAG_BACKUP_SEMANTICS,
                )
            };
            if h == INVALID_HANDLE_VALUE {
                return None;
            }
            let handle = Handle(h);
            let mut buf = vec![0u16; 1024];
            // SAFETY: valid handle and buffer length.
            let len = unsafe {
                GetFinalPathNameByHandleW(
                    handle.0,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
                )
            } as usize;
            if len == 0 || len >= buf.len() {
                return None;
            }
            let path = String::from_utf16(&buf[..len]).ok()?;
            let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
            // NTFS names compare case-insensitively.
            let prefix = path.get(..self.root.len())?;
            if !prefix.eq_ignore_ascii_case(&self.root) {
                return None;
            }
            let rest = &path[self.root.len()..];
            match rest.strip_prefix('\\') {
                Some(rel) => Some(rel.replace('\\', "/")),
                None if rest.is_empty() => Some(String::new()),
                None => None,
            }
        }

        /// The file reference number of a directory.
        pub(crate) fn dir_frn(&self, path: &Path) -> io::Result<u64> {
            let handle = open(&wide(path), FILE_READ_ATTRIBUTES)?;
            // SAFETY: all-zero is a valid BY_HANDLE_FILE_INFORMATION.
            let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: valid handle and output struct.
            if unsafe { GetFileInformationByHandle(handle.0, &mut info) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow))
        }

        /// Records since `since`, and the position after them. `None` when the journal cannot
        /// answer: another volume or root, a recreated journal, or one that wrapped past `since`.
        pub(crate) fn read(&self, since: &JournalState) -> io::Result<Option<(Vec<Record>, u64)>> {
            let data = self.query()?;
            if !since.valid
                || since.volume != self.serial
                || since.root != self.root_frn
                || since.id != data.UsnJournalID
                || (since.next_usn as i64) < data.FirstUsn
                || (since.next_usn as i64) > data.NextUsn
            {
                return Ok(None);
            }
            let end = data.NextUsn;
            let mut records = Vec::new();
            let mut usn = since.next_usn as i64;
            // u64 words keep the records 8-aligned, as the API requires.
            let mut buf = vec![0u64; 64 * 1024 / 8];
            while usn < end {
                let input = READ_USN_JOURNAL_DATA_V0 {
                    StartUsn: usn,
                    ReasonMask: u32::MAX,
                    ReturnOnlyOnClose: 0,
                    Timeout: 0,
                    BytesToWaitFor: 0,
                    UsnJournalID: data.UsnJournalID,
                };
                let mut returned = 0u32;
                let mut read = |code| {
                    // SAFETY: valid volume handle; input and output buffers with their sizes.
                    unsafe {
                        DeviceIoControl(
                            self.handle.0,
                            code,
                            (&raw const input).cast::<c_void>(),
                            size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
                            buf.as_mut_ptr().cast(),
                            (buf.len() * 8) as u32,
                            &mut returned,
                            ptr::null_mut(),
                        )
                    }
                };
                // The unprivileged code (Windows 10 1709+) first; administrators may use the classic one.
                let mut ok = read(FSCTL_READ_UNPRIVILEGED_USN_JOURNAL);
                if ok == 0 {
                    ok = read(FSCTL_READ_USN_JOURNAL);
                }
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: the buffer holds `returned` initialized bytes (u64 words reinterpreted).
                let bytes = unsafe {
                    std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), returned as usize)
                };
                if bytes.len() < 8 {
                    break;
                }
                let next = i64::from_le_bytes(bytes[..8].try_into().unwrap_or_default());
                let mut at = 8;
                while at + 60 <= bytes.len() {
                    let u32_at = |o: usize| {
                        u32::from_le_bytes(bytes[at + o..at + o + 4].try_into().unwrap_or_default())
                    };
                    let u16_at = |o: usize| {
                        u16::from_le_bytes(bytes[at + o..at + o + 2].try_into().unwrap_or_default())
                    };
                    let u64_at = |o: usize| {
                        u64::from_le_bytes(bytes[at + o..at + o + 8].try_into().unwrap_or_default())
                    };
                    let length = u32_at(0) as usize;
                    if length < 60 || at + length > bytes.len() {
                        break;
                    }
                    // USN_RECORD_V2 (NTFS). V3 (128-bit ids, ReFS) never reaches here: ReFS
                    // volumes are rejected in `open`.
                    if u16_at(4) == 2 {
                        {
                            records.push(Record {
                                // The low 48 bits are the MFT index, which is what
                                // `GetFileInformationByHandle` reports with the sequence number
                                // on top; keep both and compare the whole 64 bits.
                                frn: u64_at(8),
                                parent: u64_at(16),
                                reason: u32_at(40),
                                directory: u32_at(52) & FILE_ATTRIBUTE_DIRECTORY != 0,
                            });
                        }
                    }
                    at += length;
                }
                if next <= usn {
                    break;
                }
                usn = next;
            }
            Ok(Some((records, end as u64)))
        }
    }
}
