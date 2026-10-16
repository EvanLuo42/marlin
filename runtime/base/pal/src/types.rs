/// Access allowed to a range of committed pages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Protection {
    NoAccess,
    Read,
    ReadWrite,
    ReadExecute,
    ReadWriteExecute,
}
