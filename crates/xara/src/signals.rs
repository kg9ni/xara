#[cfg(unix)]
mod unix {
    use std::fs::File;
    use std::io::{self, Read};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use std::thread;

    #[cfg(target_os = "linux")]
    use libc::__errno_location as errno_location;
    #[cfg(not(target_os = "linux"))]
    use libc::__error as errno_location;

    static PIPE: AtomicI32 = AtomicI32::new(-1);

    extern "C" fn deliver(_: libc::c_int) {
        // SAFETY: errno is thread-local and restored before returning; write is async-signal-safe.
        unsafe {
            let errno = errno_location();
            let saved = *errno;
            libc::write(PIPE.load(Ordering::Relaxed), [0u8].as_ptr().cast(), 1);
            *errno = saved;
        }
    }

    struct Signals(File);

    fn cloexec_pipe() -> io::Result<(File, OwnedFd)> {
        let mut fds = [0; 2];
        // SAFETY: fds has room for the two descriptors pipe2 writes.
        #[cfg(not(target_os = "macos"))]
        let r = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        // SAFETY: fds has room for the two descriptors pipe writes.
        #[cfg(target_os = "macos")]
        let r = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the pipe just returned both descriptors and nothing else owns them.
        let (rx, tx) = unsafe { (File::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        #[cfg(target_os = "macos")]
        for fd in [rx.as_raw_fd(), tx.as_raw_fd()] {
            // SAFETY: fd is an open descriptor.
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok((rx, tx))
    }

    impl Signals {
        fn new(sigs: &[libc::c_int]) -> io::Result<Self> {
            let (rx, tx) = cloexec_pipe()?;
            let rx = Signals(rx);
            // SAFETY: tx is an open descriptor.
            if unsafe { libc::fcntl(tx.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } != 0 {
                return Err(io::Error::last_os_error());
            }
            PIPE.compare_exchange(-1, tx.as_raw_fd(), Ordering::SeqCst, Ordering::SeqCst)
                .map_err(|_| io::Error::other("signals are already trapped"))?;
            let _ = tx.into_raw_fd();
            for &s in sigs {
                // SAFETY: an all-zero sigaction is valid, and deliver only does async-signal-safe work.
                unsafe {
                    let mut sa: libc::sigaction = std::mem::zeroed();
                    sa.sa_sigaction = deliver as extern "C" fn(libc::c_int) as libc::sighandler_t;
                    sa.sa_flags = libc::SA_RESTART;
                    libc::sigemptyset(&mut sa.sa_mask);
                    if libc::sigaction(s, &sa, std::ptr::null_mut()) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
            }
            Ok(rx)
        }

        fn wait(&mut self) -> io::Result<()> {
            self.0.read_exact(&mut [0])
        }
    }

    pub fn trap(sigs: &[libc::c_int], stop: Arc<AtomicBool>, before_exit: fn()) -> io::Result<Arc<AtomicBool>> {
        let mut signals = Signals::new(sigs)?;
        let signaled = Arc::new(AtomicBool::new(false));
        let g = signaled.clone();
        thread::Builder::new().name("signals".into()).spawn(move || {
            while signals.wait().is_ok() {
                if g.swap(true, Ordering::SeqCst) {
                    before_exit();
                    // SAFETY: _exit ends the process at once; nothing after it relies on cleanup.
                    unsafe { libc::_exit(130) };
                }
                stop.store(true, Ordering::SeqCst);
            }
        })?;
        Ok(signaled)
    }
}

#[cfg(unix)]
pub use unix::trap;

/// Windows has no POSIX signals. Ctrl+C, Ctrl+Break, and the console closing
/// all come through one handler. The first Ctrl+C asks the modem to stop.
/// A second one, or the window closing, unkeys the radio before the process ends.
#[cfg(windows)]
mod win {
    use std::io;
    use std::process;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use windows_sys::Win32::Foundation::TRUE;
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, SetConsoleCtrlHandler,
    };

    struct Slot {
        stop: Arc<AtomicBool>,
        signaled: Arc<AtomicBool>,
        before_exit: fn(),
        hits: AtomicU32,
    }

    static SLOT: Mutex<Option<Slot>> = Mutex::new(None);

    fn take() -> Option<(Arc<AtomicBool>, Arc<AtomicBool>, fn(), u32)> {
        let g = SLOT.lock().unwrap_or_else(|e| e.into_inner());
        let s = g.as_ref()?;
        Some((s.stop.clone(), s.signaled.clone(), s.before_exit, s.hits.fetch_add(1, Ordering::SeqCst)))
    }

    unsafe extern "system" fn handler(ctrl: u32) -> i32 {
        let Some((stop, signaled, before, n)) = take() else { return 0 };
        signaled.store(true, Ordering::SeqCst);
        match ctrl {
            CTRL_C_EVENT | CTRL_BREAK_EVENT if n == 0 => {
                stop.store(true, Ordering::SeqCst);
                TRUE
            }
            CTRL_C_EVENT | CTRL_BREAK_EVENT => {
                before();
                process::exit(130);
            }
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
                before();
                stop.store(true, Ordering::SeqCst);
                TRUE
            }
            _ => 0,
        }
    }

    pub fn trap(stop: Arc<AtomicBool>, before_exit: fn()) -> io::Result<Arc<AtomicBool>> {
        let signaled = Arc::new(AtomicBool::new(false));
        *SLOT.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Slot { stop, signaled: signaled.clone(), before_exit, hits: AtomicU32::new(0) });
        // SAFETY: handler only uses the slot above, which is set before the handler is installed.
        let ok = unsafe { SetConsoleCtrlHandler(Some(handler), TRUE) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(signaled)
    }
}

#[cfg(windows)]
pub use win::trap;
