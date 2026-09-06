// Android/Bionic shims: bionic lacks glibc-isms (openpty, preadv64/pwritev64).
// Provide them so the NDK link succeeds and portable-pty + libghostty-vt work
// on Termux (aarch64-linux-android, API 21+).

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::ptr;

// Work around Bionic TLS alignment check on ARM64 (needs p_align >= 64,
// Rust's default TLS is 8). Without this, /system/bin/linker64 aborts with
// "TLS segment is underaligned: alignment is 8, needs to be at least 64".
// Use a .tdata variable with 64-byte alignment to bump PT_TLS p_align.
// See https://github.com/rust-lang/rust/issues/103666.
#[repr(align(64))]
struct Align64([u8; 64]);
#[used]
#[link_section = ".tdata"]
static TLS_ALIGN_FIX: Align64 = Align64([0; 64]);

// API 21 bionic has no preadv/pwritev at all (they appeared in API 24), and
// glibc code pulls in preadv64/pwritev64. Provide all four by emulating
// vectored I/O with a pread/pwrite loop (handles short reads correctly).
unsafe fn do_preadv(
    fd: libc::c_int,
    iov: *const libc::iovec,
    iovcnt: libc::c_int,
    mut offset: libc::off_t,
) -> libc::ssize_t {
    if iovcnt < 0 {
        *libc::__errno() = libc::EINVAL;
        return -1;
    }
    let mut total: libc::ssize_t = 0;
    for i in 0..iovcnt as isize {
        let vec = &*iov.offset(i);
        if vec.iov_len == 0 {
            continue;
        }
        let mut remaining = vec.iov_len;
        let mut base = vec.iov_base as *mut u8;
        while remaining > 0 {
            let n = libc::pread(fd, base as *mut libc::c_void, remaining, offset);
            if n < 0 {
                return if total == 0 { -1 } else { total };
            }
            if n == 0 {
                return total;
            }
            total += n;
            let n = n as usize;
            base = base.add(n);
            remaining -= n;
            offset += n as libc::off_t;
        }
    }
    total
}

unsafe fn do_pwritev(
    fd: libc::c_int,
    iov: *const libc::iovec,
    iovcnt: libc::c_int,
    mut offset: libc::off_t,
) -> libc::ssize_t {
    if iovcnt < 0 {
        *libc::__errno() = libc::EINVAL;
        return -1;
    }
    let mut total: libc::ssize_t = 0;
    for i in 0..iovcnt as isize {
        let vec = &*iov.offset(i);
        if vec.iov_len == 0 {
            continue;
        }
        let mut remaining = vec.iov_len;
        let mut base = vec.iov_base as *const u8;
        while remaining > 0 {
            let n = libc::pwrite(fd, base as *const libc::c_void, remaining, offset);
            if n < 0 {
                return if total == 0 { -1 } else { total };
            }
            total += n;
            let n = n as usize;
            base = base.add(n);
            remaining -= n;
            offset += n as libc::off_t;
        }
    }
    total
}

#[no_mangle]
pub unsafe extern "C" fn preadv(
    fd: libc::c_int,
    iov: *const libc::iovec,
    iovcnt: libc::c_int,
    offset: libc::off_t,
) -> libc::ssize_t {
    do_preadv(fd, iov, iovcnt, offset)
}
#[no_mangle]
pub unsafe extern "C" fn pwritev(
    fd: libc::c_int,
    iov: *const libc::iovec,
    iovcnt: libc::c_int,
    offset: libc::off_t,
) -> libc::ssize_t {
    do_pwritev(fd, iov, iovcnt, offset)
}
#[no_mangle]
pub unsafe extern "C" fn preadv64(
    fd: libc::c_int,
    iov: *const libc::iovec,
    iovcnt: libc::c_int,
    offset: libc::off_t,
) -> libc::ssize_t {
    do_preadv(fd, iov, iovcnt, offset)
}
#[no_mangle]
pub unsafe extern "C" fn pwritev64(
    fd: libc::c_int,
    iov: *const libc::iovec,
    iovcnt: libc::c_int,
    offset: libc::off_t,
) -> libc::ssize_t {
    do_pwritev(fd, iov, iovcnt, offset)
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
