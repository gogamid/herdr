// Android/Bionic shims: bionic lacks glibc-isms (openpty, preadv64/pwritev64).
// Provide them so the NDK link succeeds and portable-pty + libghostty-vt work
// on Termux (aarch64-linux-android, API 21+).

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::ptr;

// bionic has preadv/pwritev but not the *64 aliases glibc exposes.
// Rust crates / zig-built C may reference preadv64/pwritev64. Alias to the
// non-64 variants (on LP64 Android, off_t is already 64-bit).
#[no_mangle]
pub unsafe extern "C" fn preadv64(
    fd: libc::c_int,
    iov: *const libc::iovec,
    iovcnt: libc::c_int,
    offset: libc::off_t,
) -> libc::ssize_t {
    // libc::preadv exists on Android; fall back to it. Signature matches
    // on 64-bit (aarch64 API21+ off_t == off64_t).
    libc::preadv(fd, iov, iovcnt, offset)
}

#[no_mangle]
pub unsafe extern "C" fn pwritev64(
    fd: libc::c_int,
    iov: *const libc::iovec,
    iovcnt: libc::c_int,
    offset: libc::off_t,
) -> libc::ssize_t {
    libc::pwritev(fd, iov, iovcnt, offset)
}

// bionic has no openpty(3) (it's in libutil on glibc/BSD). Implement it via
// posix_openpt + grantpt + unlockpt + ptsname + open(slave).
#[no_mangle]
pub unsafe extern "C" fn openpty(
    amaster: *mut libc::c_int,
    aslave: *mut libc::c_int,
    name: *mut libc::c_char,
    termp: *const libc::termios,
    winp: *const libc::winsize,
) -> libc::c_int {
    if amaster.is_null() || aslave.is_null() {
        // bionic's errno is via __errno(); libc crate exposes it differently
        // per target — just set via std::io::Error's last_os_error indirect
        // by returning -1 with EINVAL; caller will check errno.
        unsafe { *libc::__errno() = libc::EINVAL };
        return -1;
    }

    let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
    if master < 0 {
        return -1;
    }
    if libc::grantpt(master) != 0 || libc::unlockpt(master) != 0 {
        libc::close(master);
        return -1;
    }

    let slave_name_ptr = libc::ptsname(master);
    if slave_name_ptr.is_null() {
        libc::close(master);
        return -1;
    }
    let cstr = CStr::from_ptr(slave_name_ptr);
    // Keep an owned copy for the open() + optional name out-param.
    let slave_path = match CString::new(cstr.to_bytes()) {
        Ok(s) => s,
        Err(_) => {
            libc::close(master);
            return -1;
        }
    };

    // Open slave side. Use O_RDWR|O_NOCTTY; the child will do setsid+TIOCSCTTY.
    let slave = libc::open(slave_path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
    if slave < 0 {
        libc::close(master);
        return -1;
    }

    // Apply termios / window size if caller supplied them.
    if !termp.is_null() {
        // tcsetattr on the slave; ignore errors (glibc does the same best-effort).
        let _ = libc::tcsetattr(slave, libc::TCSANOW, termp);
    }
    if !winp.is_null() {
        let _ = libc::ioctl(slave, libc::TIOCSWINSZ as _, winp);
        let _ = libc::ioctl(master, libc::TIOCSWINSZ as _, winp);
    }

    if !name.is_null() {
        // glibc copies the slave name into `name` if non-null. We do the same,
        // truncated to fit if needed (glibc guarantees at least 64 bytes when
        // callers pass a buffer, but we just respect what we can).
        // The safest portable thing: copy up to strlen(slave_path) + 1.
        let bytes = slave_path.as_bytes_with_nul();
        ptr::copy_nonoverlapping(bytes.as_ptr() as *const libc::c_char, name, bytes.len());
    }

    *amaster = master;
    *aslave = slave;
    0
}
