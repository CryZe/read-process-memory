//! Read memory from another process' address space.
//!
//! This crate provides a trait—[`CopyAddress`](trait.CopyAddress.html),
//! and a helper function—[`copy_address`](fn.copy_address.html) that
//! allow reading memory from another process.
//!
//! Note: you may not always have permission to read memory from another
//! process! This may require `sudo` on some systems, and may fail even with
//! `sudo` on macOS. You are most likely to succeed if you are attempting to
//! read a process that you have spawned yourself.
//!
//! # Examples
//!
//! ```rust,no_run
//! # use std::convert::TryInto;
//! # use std::io;
//! use read_process_memory::*;
//!
//! # fn foo(pid: Pid, address: usize, size: usize) -> io::Result<()> {
//! let handle: ProcessHandle = pid.try_into()?;
//! let bytes = copy_address(address, size, &handle)?;
//! # Ok(())
//! # }
//! ```

#[doc(hidden)]
#[doc = include_str!("../README.md")]
mod readme {}

use std::io;

/// A trait that provides a method for reading memory from another process.
pub trait CopyAddress {
    /// Try to copy `buf.len()` bytes from `addr` in the process `self`, placing
    /// them in `buf`.
    fn copy_address(&self, addr: usize, buf: &mut [u8]) -> io::Result<()>;
}

/// A process ID.
pub use crate::platform::Pid;
/// A handle to a running process. This is not a process ID on all platforms.
///
/// For convenience, this crate implements `TryFrom`-backed conversions from
/// `Pid` to `ProcessHandle`.
///
/// # Examples
///
/// ```rust,no_run
/// use read_process_memory::*;
/// use std::convert::TryInto;
/// use std::io;
///
/// fn pid_to_handle(pid: Pid) -> io::Result<ProcessHandle> {
///     Ok(pid.try_into()?)
/// }
/// ```
///
/// This operation is not guaranteed to succeed. Specifically, on Windows
/// `OpenProcess` may fail. On macOS `task_for_pid` will generally fail
/// unless run as root, and even then it may fail when called on certain
/// programs; it may however run without root on the current process.
pub use crate::platform::ProcessHandle;

#[cfg(target_os = "linux")]
mod platform {
    use libc::{c_void, iovec, pid_t, process_vm_readv};
    use std::convert::TryFrom;
    use std::fs;
    use std::io;
    use std::io::Read;
    use std::io::Seek;
    use std::process::Child;

    use super::CopyAddress;

    /// On Linux a `Pid` is just a `libc::pid_t`.
    pub type Pid = pid_t;
    /// On Linux a `ProcessHandle` is just a `libc::pid_t`.
    #[derive(Clone)]
    pub struct ProcessHandle(Pid);

    /// On Linux, process handle is a pid.
    impl TryFrom<Pid> for ProcessHandle {
        type Error = io::Error;

        fn try_from(pid: Pid) -> io::Result<Self> {
            Ok(Self(pid))
        }
    }

    /// A `process::Child` always has a pid, which is all we need on Linux.
    impl TryFrom<&Child> for ProcessHandle {
        type Error = io::Error;

        fn try_from(child: &Child) -> io::Result<Self> {
            Self::try_from(child.id() as Pid)
        }
    }

    impl CopyAddress for ProcessHandle {
        fn copy_address(&self, addr: usize, buf: &mut [u8]) -> io::Result<()> {
            let local_iov = iovec {
                iov_base: buf.as_mut_ptr() as *mut c_void,
                iov_len: buf.len(),
            };
            let remote_iov = iovec {
                iov_base: addr as *mut c_void,
                iov_len: buf.len(),
            };
            let result = unsafe { process_vm_readv(self.0, &local_iov, 1, &remote_iov, 1, 0) };
            if result == -1 {
                match io::Error::last_os_error().raw_os_error() {
                    Some(libc::ENOSYS) | Some(libc::EPERM) => {
                        // fallback to reading /proc/$pid/mem if kernel does not
                        // implement process_vm_readv()
                        let mut procmem = fs::File::open(format!("/proc/{}/mem", self.0))?;
                        procmem.seek(io::SeekFrom::Start(addr as u64))?;
                        procmem.read_exact(buf)
                    }
                    _ => Err(io::Error::last_os_error()),
                }
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use libc::{c_int, pid_t};
    use mach::kern_return::{kern_return_t, KERN_SUCCESS};
    use mach::port::{mach_port_name_t, mach_port_t, MACH_PORT_NULL};
    use mach::vm_types::{mach_vm_address_t, mach_vm_size_t};

    use std::convert::TryFrom;
    use std::io;
    use std::process::Child;

    use super::CopyAddress;

    #[allow(non_camel_case_types)]
    type vm_map_t = mach_port_t;
    #[allow(non_camel_case_types)]
    type vm_address_t = mach_vm_address_t;
    #[allow(non_camel_case_types)]
    type vm_size_t = mach_vm_size_t;

    /// On macOS a `Pid` is just a `libc::pid_t`.
    pub type Pid = pid_t;
    /// On macOS a `ProcessHandle` is a mach port.
    #[derive(Clone)]
    pub struct ProcessHandle(mach_port_name_t);

    extern "C" {
        fn vm_read_overwrite(
            target_task: vm_map_t,
            address: vm_address_t,
            size: vm_size_t,
            data: vm_address_t,
            out_size: *mut vm_size_t,
        ) -> kern_return_t;
    }

    /// A small wrapper around `task_for_pid`, which takes a pid and returns the
    /// mach port representing its task.
    fn task_for_pid(pid: Pid) -> io::Result<mach_port_name_t> {
        if pid == unsafe { libc::getpid() } as Pid {
            return Ok(unsafe { mach::traps::mach_task_self() });
        }

        let mut task: mach_port_name_t = MACH_PORT_NULL;

        unsafe {
            let result =
                mach::traps::task_for_pid(mach::traps::mach_task_self(), pid as c_int, &mut task);
            if result != KERN_SUCCESS {
                return Err(io::Error::last_os_error());
            }
        }

        Ok(task)
    }

    /// A `Pid` can be turned into a `ProcessHandle` with `task_for_pid`.
    impl TryFrom<Pid> for ProcessHandle {
        type Error = io::Error;

        fn try_from(pid: Pid) -> io::Result<Self> {
            Ok(Self(task_for_pid(pid)?))
        }
    }

    /// On Darwin, process handle is a mach port name.
    impl TryFrom<mach_port_name_t> for ProcessHandle {
        type Error = io::Error;

        fn try_from(mach_port_name: mach_port_name_t) -> io::Result<Self> {
            Ok(Self(mach_port_name))
        }
    }

    /// This `TryFrom` impl simply calls the `TryFrom` impl for `Pid`.
    ///
    /// Unfortunately spawning a process on macOS does not hand back a mach
    /// port by default (you have to jump through several hoops to get at it),
    /// so there's no simple implementation of `TryFrom` Child
    /// `for::Child`. This implementation is just provided for symmetry
    /// with other platforms to make writing cross-platform code easier.
    ///
    /// Ideally we would provide an implementation of
    /// `std::process::Command::spawn` that jumped through those hoops and
    /// provided the task port.
    impl TryFrom<&Child> for ProcessHandle {
        type Error = io::Error;

        fn try_from(child: &Child) -> io::Result<Self> {
            Self::try_from(child.id() as Pid)
        }
    }

    /// Use `vm_read` to read memory from another process on macOS.
    impl CopyAddress for ProcessHandle {
        fn copy_address(&self, addr: usize, buf: &mut [u8]) -> io::Result<()> {
            let mut read_len = buf.len() as vm_size_t;
            let result = unsafe {
                vm_read_overwrite(
                    self.0,
                    addr as vm_address_t,
                    buf.len() as vm_size_t,
                    buf.as_mut_ptr() as vm_address_t,
                    &mut read_len,
                )
            };

            if read_len != buf.len() as vm_size_t {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!(
                        "Mismatched read sizes for `vm_read` (expected {}, got {})",
                        buf.len(),
                        read_len
                    ),
                ));
            }

            if result != KERN_SUCCESS {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }
}

#[cfg(target_os = "freebsd")]
mod platform {
    use libc::{c_int, c_void, pid_t};
    use libc::{waitpid, EBUSY, PIOD_READ_D, PT_ATTACH, PT_DETACH, PT_IO, WIFSTOPPED};
    use std::convert::TryFrom;
    use std::process::Child;
    use std::{io, ptr};

    use super::CopyAddress;

    /// On FreeBSD a `Pid` is just a `libc::pid_t`.
    pub type Pid = pid_t;
    /// On FreeBSD a `ProcessHandle` is just a `libc::pid_t`.
    #[derive(Clone)]
    pub struct ProcessHandle(Pid);

    #[repr(C)]
    struct PtraceIoDesc {
        piod_op: c_int,
        piod_offs: *mut c_void,
        piod_addr: *mut c_void,
        piod_len: usize,
    }

    /// If process is already traced, PT_ATTACH call returns
    /// EBUSY. This structure is needed to avoid double locking the process.
    /// - `Release` variant means we can safely detach from the process.
    /// - `NoRelease` variant means that process was already attached, so we
    ///   shall not attempt to detach from it.
    #[derive(PartialEq)]
    enum PtraceLockState {
        Release,
        NoRelease,
    }

    extern "C" {
        /// libc version of ptrace takes *mut i8 as third argument,
        /// which is not very ergonomic if we have a struct.
        fn ptrace(request: c_int, pid: pid_t, io_desc: *const PtraceIoDesc, data: c_int) -> c_int;
    }

    /// On FreeBSD, process handle is a pid.
    impl TryFrom<Pid> for ProcessHandle {
        type Error = io::Error;

        fn try_from(pid: Pid) -> io::Result<Self> {
            Ok(Self(pid))
        }
    }

    /// A `process::Child` always has a pid, which is all we need on FreeBSD.
    impl TryFrom<&Child> for ProcessHandle {
        type Error = io::Error;

        fn try_from(child: &Child) -> io::Result<Self> {
            Self::try_from(child.id() as Pid)
        }
    }

    /// Attach to a process `pid` and wait for the process to be stopped.
    fn ptrace_attach(pid: Pid) -> io::Result<PtraceLockState> {
        let attach_status = unsafe { ptrace(PT_ATTACH, pid, ptr::null_mut(), 0) };

        let last_error = io::Error::last_os_error();

        if let Some(error) = last_error.raw_os_error() {
            if attach_status == -1 {
                return match error {
                    EBUSY => Ok(PtraceLockState::NoRelease),
                    _ => Err(last_error),
                };
            }
        }

        let mut wait_status = 0;

        let stopped = unsafe {
            waitpid(pid, &mut wait_status as *mut _, 0);
            WIFSTOPPED(wait_status)
        };

        if !stopped {
            Err(io::Error::last_os_error())
        } else {
            Ok(PtraceLockState::Release)
        }
    }

    /// Read process `pid` memory at `addr` to `buf` via PT_IO ptrace call.
    fn ptrace_io(pid: Pid, addr: usize, buf: &mut [u8]) -> io::Result<()> {
        let ptrace_io_desc = PtraceIoDesc {
            piod_op: PIOD_READ_D,
            piod_offs: addr as *mut c_void,
            piod_addr: buf.as_mut_ptr() as *mut c_void,
            piod_len: buf.len(),
        };

        let result = unsafe { ptrace(PT_IO, pid, &ptrace_io_desc as *const _, 0) };

        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Detach from the process `pid`.
    fn ptrace_detach(pid: Pid) -> io::Result<()> {
        let detach_status = unsafe { ptrace(PT_DETACH, pid, ptr::null_mut(), 0) };

        if detach_status == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    impl CopyAddress for ProcessHandle {
        fn copy_address(&self, addr: usize, buf: &mut [u8]) -> io::Result<()> {
            let should_detach = ptrace_attach(self.0)? == PtraceLockState::Release;

            let result = ptrace_io(self.0, addr, buf);
            if should_detach {
                ptrace_detach(self.0)?
            }
            result
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::convert::TryFrom;
    use std::io;
    use std::mem;
    use std::ops::Deref;
    use std::os::windows::io::{AsRawHandle, RawHandle};
    use std::process::Child;
    use std::ptr;
    use std::sync::Arc;
    use winapi::{
        shared::{basetsd, minwindef},
        um::{handleapi, memoryapi, processthreadsapi, winnt},
    };

    use super::CopyAddress;

    /// On Windows a `Pid` is a `DWORD`.
    pub type Pid = minwindef::DWORD;
    #[derive(Eq, PartialEq, Hash)]
    struct ProcessHandleInner(RawHandle);
    /// On Windows a `ProcessHandle` is a `HANDLE`.
    #[derive(Clone, Eq, PartialEq, Hash)]
    pub struct ProcessHandle(Arc<ProcessHandleInner>);

    impl Deref for ProcessHandle {
        type Target = RawHandle;

        fn deref(&self) -> &Self::Target {
            &self.0 .0
        }
    }

    impl Drop for ProcessHandleInner {
        fn drop(&mut self) {
            unsafe { handleapi::CloseHandle(self.0) };
        }
    }

    /// A `Pid` can be turned into a `ProcessHandle` with `OpenProcess`.
    impl TryFrom<Pid> for ProcessHandle {
        type Error = io::Error;

        fn try_from(pid: Pid) -> io::Result<Self> {
            let handle = unsafe { processthreadsapi::OpenProcess(winnt::PROCESS_VM_READ, 0, pid) };
            if handle == (0 as RawHandle) {
                Err(io::Error::last_os_error())
            } else {
                Ok(Self(Arc::new(ProcessHandleInner(handle))))
            }
        }
    }

    /// A `std::process::Child` has a `HANDLE` from calling `CreateProcess`.
    impl TryFrom<&Child> for ProcessHandle {
        type Error = io::Error;

        fn try_from(child: &Child) -> io::Result<Self> {
            Ok(Self(Arc::new(ProcessHandleInner(child.as_raw_handle()))))
        }
    }

    impl From<RawHandle> for ProcessHandle {
        fn from(handle: RawHandle) -> Self {
            return Self(Arc::new(ProcessHandleInner(handle)));
        }
    }

    /// Use `ReadProcessMemory` to read memory from another process on Windows.
    impl CopyAddress for ProcessHandle {
        fn copy_address(&self, addr: usize, buf: &mut [u8]) -> io::Result<()> {
            if buf.len() == 0 {
                return Ok(());
            }

            if unsafe {
                memoryapi::ReadProcessMemory(
                    self.0 .0,
                    addr as minwindef::LPVOID,
                    buf.as_mut_ptr() as minwindef::LPVOID,
                    mem::size_of_val(buf) as basetsd::SIZE_T,
                    ptr::null_mut(),
                )
            } == 0
            {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }
}

/// Copy `length` bytes of memory at `addr` from `source`.
///
/// This is just a convenient way to call `CopyAddress::copy_address` without
/// having to provide your own buffer.
pub fn copy_address<T>(addr: usize, length: usize, source: &T) -> io::Result<Vec<u8>>
where
    T: CopyAddress,
{
    log::debug!("copy_address: addr: {:x}", addr);

    let mut copy = vec![0; length];

    source
        .copy_address(addr, &mut copy)
        .map_err(|e| {
            log::warn!("copy_address failed for {:x}: {:?}", addr, e);
            e
        })
        .and(Ok(copy))
}

#[cfg(test)]
mod test {
    use super::*;
    use std::convert::TryFrom;
    use std::env;
    use std::io::{self, BufRead, BufReader};
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};

    fn test_process_path() -> Option<PathBuf> {
        env::current_exe().ok().and_then(|p| {
            p.parent().map(|p| {
                p.with_file_name("test")
                    .with_extension(env::consts::EXE_EXTENSION)
            })
        })
    }

    fn spawn_with_handle(cmd: &mut Command) -> io::Result<(Child, ProcessHandle)> {
        let child = cmd.spawn()?;
        let handle = ProcessHandle::try_from(child.id() as Pid)?;
        Ok((child, handle))
    }

    fn read_test_process(args: Option<&[&str]>) -> io::Result<Vec<u8>> {
        // Spawn a child process and attempt to read its memory.
        let path = test_process_path().unwrap();
        let mut cmd = Command::new(&path);
        {
            cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        }
        if let Some(a) = args {
            cmd.args(a);
        }
        let (mut child, handle) = spawn_with_handle(&mut cmd)?;
        // The test program prints the address and size.
        // See `src/bin/test.rs` for its source.
        let reader = BufReader::new(child.stdout.take().unwrap());
        let line = reader.lines().next().unwrap().unwrap();
        let bits = line.split(' ').collect::<Vec<_>>();
        let addr = usize::from_str_radix(&bits[0][2..], 16).unwrap();
        let size = bits[1].parse::<usize>().unwrap();
        let mem = copy_address(addr, size, &handle)?;
        child.wait()?;
        Ok(mem)
    }

    #[test]
    fn test_read_small() {
        let mem = read_test_process(None).unwrap();
        assert_eq!(mem, (0..32u8).collect::<Vec<u8>>());
    }

    #[test]
    fn test_read_large() {
        // 20,000 should be greater than a single page on most systems.
        // macOS on ARM is 16384.
        const SIZE: usize = 20_000;
        let arg = format!("{}", SIZE);
        let mem = read_test_process(Some(&[&arg])).unwrap();
        let expected = (0..SIZE)
            .map(|v| (v % (u8::max_value() as usize + 1)) as u8)
            .collect::<Vec<u8>>();
        assert_eq!(mem, expected);
    }
}
