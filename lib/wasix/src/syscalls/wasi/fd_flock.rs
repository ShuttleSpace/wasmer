use super::*;
use crate::syscalls::*;

/// ### `fd_flock()`
/// Advisory file locking (intra-process, WASM sandbox).
/// Inputs:
/// - `Fd fd`
///     The file descriptor to lock/unlock
/// - `i32 operation`
///     Bitmask: LOCK_SH=1, LOCK_EX=2, LOCK_NB=4, LOCK_UN=8
///
/// Returns Errno::Success on success, Errno::Again if LOCK_NB and lock is held.
#[instrument(level = "trace", skip_all, fields(%fd, %operation), ret)]
pub fn fd_flock(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    fd: WasiFd,
    operation: i32,
) -> Result<Errno, WasiError> {
    WasiEnv::do_pending_operations(&mut ctx)?;

    let env = ctx.data();
    let (_, state) = unsafe { env.get_memory_and_wasi_state(&ctx, 0) };

    // Resolve fd -> inode number
    let inode_id = {
        let fd_entry = match state.fs.get_fd(fd) {
            Ok(f) => f,
            Err(e) => return Ok(e),
        };
        fd_entry.inode.ino().as_u64()
    };

    const LOCK_SH: i32 = 1;
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    const LOCK_UN: i32 = 8;

    let lock_type = operation & !LOCK_NB;
    let nonblocking = (operation & LOCK_NB) != 0;

    let mut locks = state.file_locks.lock().unwrap();
    let entry = locks
        .entry(inode_id)
        .or_insert_with(|| crate::state::FileLockState::default());

    match lock_type {
        x if x == LOCK_UN => {
            // Release: decrement readers or clear exclusive
            if entry.lock_type == 1 && entry.readers > 0 {
                entry.readers -= 1;
                if entry.readers == 0 {
                    entry.lock_type = 0;
                }
            } else {
                entry.lock_type = 0;
                entry.readers = 0;
            }
            Ok(Errno::Success)
        }
        x if x == LOCK_SH => {
            // Shared lock: only conflicts with exclusive
            if entry.lock_type == 2 {
                if nonblocking {
                    return Ok(Errno::Again);
                }
                // In WASM single-process context: no other thread holds it,
                // so just proceed (advisory only)
            }
            entry.lock_type = 1;
            entry.readers += 1;
            Ok(Errno::Success)
        }
        x if x == LOCK_EX => {
            // Exclusive lock: conflicts with any existing lock
            if entry.lock_type != 0 {
                if nonblocking {
                    return Ok(Errno::Again);
                }
                // Advisory lock in single WASM process: just grant it
            }
            entry.lock_type = 2;
            entry.readers = 0;
            Ok(Errno::Success)
        }
        _ => Ok(Errno::Inval),
    }
}
