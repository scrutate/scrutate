//! Lazy binding for system functions a plain search never calls (macOS on Apple silicon).
//!
//! dyld binds every imported symbol before `main`, about 3.3 µs each when the symbol comes from
//! libSystem (it looks through the libraries libSystem re-exports). SQLite, PCRE2, process
//! spawning, the unwinder and the C runtime checks import over a hundred functions that
//! `scrutate grep` never reaches: 0.4 ms of every 2.4 ms start. Each name below is defined here
//! instead, as a stub that finds the real function on its first call, stores it and jumps to it,
//! so the executable no longer imports it. The stubs are private to the executable; libSystem and
//! other libraries keep their own bindings.
//!
//! Not listed, on purpose: data symbols, what the runtime calls while starting or exiting, and
//! what a search itself calls (a lazy lookup costs as much as dyld's). `fork` binds every stub
//! first: a lookup in the child could wait forever on a lock another thread held at the fork.

use std::ffi::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
    fn abort() -> !;
    // the stubs' (name, slot) pairs: the linker's names for the bounds of their section
    #[link_name = "\u{1}section$start$__DATA$__scr_lazy"]
    static TABLE_START: Entry;
    #[link_name = "\u{1}section$end$__DATA$__scr_lazy"]
    static TABLE_END: Entry;
}

#[repr(C)]
struct Entry {
    name: *const c_char,
    slot: *const AtomicPtr<c_void>,
}

const RTLD_LAZY: c_int = 0x1;
const RTLD_NOLOAD: c_int = 0x10;
/// Every loaded image; the stubs are not exported, so this never finds one of them.
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

unsafe fn system() -> *mut c_void {
    unsafe {
        static SYSTEM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
        let mut lib = SYSTEM.load(Ordering::Relaxed);
        if lib.is_null() {
            lib = dlopen(c"/usr/lib/libSystem.B.dylib".as_ptr(), RTLD_LAZY | RTLD_NOLOAD);
            if lib.is_null() {
                lib = RTLD_DEFAULT;
            }
            SYSTEM.store(lib, Ordering::Relaxed);
        }
        lib
    }
}

unsafe fn find(name: *const c_char) -> *mut c_void {
    unsafe {
        let f = dlsym(system(), name);
        if f.is_null() {
            // nothing here may allocate or format: the caller can be any C function
            let msg = b"scrutate: a system function is missing: ";
            write(2, msg.as_ptr().cast(), msg.len());
            let mut n = 0;
            while *name.add(n) != 0 {
                n += 1;
            }
            write(2, name.cast(), n);
            write(2, b"\n".as_ptr().cast(), 1);
            abort();
        }
        f
    }
}

/// Called by `_scrutate_lazy_bind` with a stub's name and slot; returns the function to jump to.
#[unsafe(no_mangle)]
unsafe extern "C" fn scrutate_lazy_resolve(name: *const c_char, slot: *const AtomicPtr<c_void>) -> *mut c_void {
    unsafe {
        let f = find(name);
        (*slot).store(f, Ordering::Relaxed);
        f
    }
}

/// The stubs not bound yet (for the tests).
#[allow(dead_code)]
pub fn unbound() -> usize {
    table().iter().filter(|e| unsafe { (*e.slot).load(Ordering::Relaxed).is_null() }).count()
}

fn table() -> &'static [Entry] {
    unsafe {
        let start = std::ptr::addr_of!(TABLE_START);
        let n = (std::ptr::addr_of!(TABLE_END) as usize - start as usize) / std::mem::size_of::<Entry>();
        std::slice::from_raw_parts(start, n)
    }
}

/// `fork`, as the executable sees it.
#[unsafe(no_mangle)]
unsafe extern "C" fn scrutate_lazy_fork() -> c_int {
    unsafe {
        for e in table() {
            if (*e.slot).load(Ordering::Relaxed).is_null() {
                scrutate_lazy_resolve(e.name, e.slot);
            }
        }
        let fork: unsafe extern "C" fn() -> c_int = std::mem::transmute(find(c"fork".as_ptr()));
        fork()
    }
}

// `_scrutate_lazy_bind` keeps every argument register (x0-x7, the result pointer x8, q0-q7) and
// the stack as the caller left them, so it works for any signature, variadic ones included. x16
// and x17 are the two registers a call may clobber on the way to its target.
core::arch::global_asm!(
    "    .section __TEXT,__text,regular,pure_instructions",
    "    .p2align 2",
    "_scrutate_lazy_bind:",
    "    stp x29, x30, [sp, #-224]!",
    "    mov x29, sp",
    "    stp x0, x1, [sp, #16]",
    "    stp x2, x3, [sp, #32]",
    "    stp x4, x5, [sp, #48]",
    "    stp x6, x7, [sp, #64]",
    "    str x8, [sp, #80]",
    "    stp q0, q1, [sp, #96]",
    "    stp q2, q3, [sp, #128]",
    "    stp q4, q5, [sp, #160]",
    "    stp q6, q7, [sp, #192]",
    "    mov x0, x16",
    "    mov x1, x17",
    "    bl _scrutate_lazy_resolve",
    "    mov x17, x0",
    "    ldp q6, q7, [sp, #192]",
    "    ldp q4, q5, [sp, #160]",
    "    ldp q2, q3, [sp, #128]",
    "    ldp q0, q1, [sp, #96]",
    "    ldr x8, [sp, #80]",
    "    ldp x6, x7, [sp, #64]",
    "    ldp x4, x5, [sp, #48]",
    "    ldp x2, x3, [sp, #32]",
    "    ldp x0, x1, [sp, #16]",
    "    ldp x29, x30, [sp], #224",
    "    br x17",
    "    .private_extern _fork",
    "    .globl _fork",
    "    .p2align 2",
    "_fork:",
    "    b _scrutate_lazy_fork",
);

macro_rules! lazy {
    ($($label:ident = $name:literal),* $(,)?) => {
        $(core::arch::global_asm!(concat!(
            "    .section __TEXT,__text,regular,pure_instructions\n",
            "    .private_extern \"_", $name, "\"\n",
            "    .globl \"_", $name, "\"\n",
            "    .p2align 2\n",
            "\"_", $name, "\":\n",
            "    adrp x16, _scrutate_lazy_slot_", stringify!($label), "@PAGE\n",
            "    ldr x17, [x16, _scrutate_lazy_slot_", stringify!($label), "@PAGEOFF]\n",
            "    cbz x17, 1f\n",
            "    br x17\n",
            "1:  add x17, x16, _scrutate_lazy_slot_", stringify!($label), "@PAGEOFF\n",
            "    adrp x16, _scrutate_lazy_name_", stringify!($label), "@PAGE\n",
            "    add x16, x16, _scrutate_lazy_name_", stringify!($label), "@PAGEOFF\n",
            "    b _scrutate_lazy_bind\n",
            "    .section __DATA,__data\n",
            "    .p2align 3\n",
            "_scrutate_lazy_slot_", stringify!($label), ":\n",
            "    .quad 0\n",
            "    .section __TEXT,__const\n",
            "_scrutate_lazy_name_", stringify!($label), ":\n",
            "    .asciz \"", $name, "\"\n",
            "    .section __DATA,__scr_lazy,regular,no_dead_strip\n",
            "    .p2align 3\n",
            "    .quad _scrutate_lazy_name_", stringify!($label), "\n",
            "    .quad _scrutate_lazy_slot_", stringify!($label), "\n",
        ));)*
    };
}

lazy! {
    // C runtime checks and character classes (SQLite, PCRE2, tree-sitter scanners)
    assert_rtn = "__assert_rtn", maskrune = "__maskrune", memcpy_chk = "__memcpy_chk", memmove_chk = "__memmove_chk",
    memset_chk = "__memset_chk", strlcat_chk = "__strlcat_chk", strlcpy_chk = "__strlcpy_chk", tolower = "__tolower",
    toupper = "__toupper", atoi = "atoi", qsort = "qsort", random = "random", srandomdev = "srandomdev", memchr = "memchr",
    memset_pattern16 = "memset_pattern16", strchr = "strchr", strcmp = "strcmp", strcspn = "strcspn", strncmp = "strncmp",
    strncpy = "strncpy", strrchr = "strrchr", strspn = "strspn", snprintf = "snprintf", vsnprintf = "vsnprintf",
    strerror_r = "strerror_r",
    // stdio
    fclose = "fclose", fdopen = "fdopen$DARWIN_EXTSN", fprintf = "fprintf", fputc = "fputc", fputs = "fputs", fwrite = "fwrite",
    // files, as SQLite and the index use them
    access = "access", dup = "dup", fchmod = "fchmod", fchown = "fchown", fdopendir = "fdopendir", flock = "flock",
    fsctl = "fsctl", fsetattrlist = "fsetattrlist", fstatat = "fstatat", fstatfs = "fstatfs", fsync = "fsync",
    ftruncate = "ftruncate", futimes = "futimes", ioctl = "ioctl", lstat = "lstat", madvise = "madvise", mkdir = "mkdir",
    openat = "openat", posix_madvise = "posix_madvise", pwrite = "pwrite", readlink = "readlink", readv = "readv",
    rename = "rename", rmdir = "rmdir", statfs = "statfs", unlink = "unlink", unlinkat = "unlinkat", utimes = "utimes",
    writev = "writev",
    // users, hosts, time
    confstr = "confstr", geteuid = "geteuid", gethostuuid = "gethostuuid", getpid = "getpid", getpwuid_r = "getpwuid_r",
    getuid = "getuid", gettimeofday = "gettimeofday", localtime_r = "localtime_r", time = "time", setenv = "setenv",
    dispatch_time = "dispatch_time", pause = "pause",
    // loading, allocation zones, generated code
    dlclose = "dlclose", dlerror = "dlerror", malloc_create_zone = "malloc_create_zone", malloc_default_zone = "malloc_default_zone",
    malloc_set_zone_name = "malloc_set_zone_name", malloc_size = "malloc_size", malloc_zone_free = "malloc_zone_free",
    malloc_zone_malloc = "malloc_zone_malloc", malloc_zone_realloc = "malloc_zone_realloc",
    pthread_jit_write_protect_np = "pthread_jit_write_protect_np", sys_icache_invalidate = "sys_icache_invalidate",
    // math
    expm1 = "expm1", log = "log", log1p = "log1p", powf = "powf",
    // starting other programs
    pipe = "pipe", posix_spawn_file_actions_adddup2 = "posix_spawn_file_actions_adddup2",
    posix_spawn_file_actions_destroy = "posix_spawn_file_actions_destroy", posix_spawn_file_actions_init = "posix_spawn_file_actions_init",
    posix_spawnattr_destroy = "posix_spawnattr_destroy", posix_spawnattr_init = "posix_spawnattr_init",
    posix_spawnattr_setflags = "posix_spawnattr_setflags", posix_spawnattr_setpgroup = "posix_spawnattr_setpgroup",
    posix_spawnattr_setsigdefault = "posix_spawnattr_setsigdefault", posix_spawnp = "posix_spawnp", waitpid = "waitpid",
    chdir = "chdir", chroot = "chroot", dup2 = "dup2", execvp = "execvp", setgid = "setgid", setgroups = "setgroups",
    setpgid = "setpgid", setsid = "setsid", setuid = "setuid", sigaddset = "sigaddset", sigemptyset = "sigemptyset",
    // threads, beyond what every search uses
    pthread_cond_broadcast = "pthread_cond_broadcast", pthread_cond_destroy = "pthread_cond_destroy",
    pthread_cond_signal = "pthread_cond_signal", pthread_cond_wait = "pthread_cond_wait", pthread_detach = "pthread_detach",
    sched_yield = "sched_yield",
    // what a search of one file never calls: the threads a larger tree starts, the count of big
    // searches running, a canonical path
    pthread_create = "pthread_create", pthread_join = "pthread_join", pthread_attr_init = "pthread_attr_init",
    pthread_attr_destroy = "pthread_attr_destroy", pthread_attr_setstacksize = "pthread_attr_setstacksize", nanosleep = "nanosleep",
    semget = "semget", semop = "semop", semctl = "semctl", realpath = "realpath$DARWIN_EXTSN",
    dispatch_semaphore_signal = "dispatch_semaphore_signal", dispatch_semaphore_wait = "dispatch_semaphore_wait",
    dispatch_release = "dispatch_release", lseek = "lseek",
    // a session's socket, and the command line call that asks it
    socket = "socket", bind = "bind", listen = "listen", accept = "accept", connect = "connect", getpeereid = "getpeereid",
    setsockopt = "setsockopt", recv = "recv", send = "send", chmod = "chmod", ns_get_executable_path = "_NSGetExecutablePath", fchdir = "fchdir",
    pthread_cond_timedwait_relative_np = "pthread_cond_timedwait_relative_np",
    // panics: unwinding and the backtrace
    unwind_backtrace = "_Unwind_Backtrace", unwind_delete_exception = "_Unwind_DeleteException", unwind_get_cfa = "_Unwind_GetCFA",
    unwind_get_data_rel_base = "_Unwind_GetDataRelBase", unwind_get_ip = "_Unwind_GetIP", unwind_get_ip_info = "_Unwind_GetIPInfo",
    unwind_get_lsda = "_Unwind_GetLanguageSpecificData", unwind_get_region_start = "_Unwind_GetRegionStart",
    unwind_get_text_rel_base = "_Unwind_GetTextRelBase", unwind_raise_exception = "_Unwind_RaiseException",
    unwind_resume = "_Unwind_Resume", unwind_set_gr = "_Unwind_SetGR", unwind_set_ip = "_Unwind_SetIP",
    dyld_get_image_header = "_dyld_get_image_header", dyld_get_image_name = "_dyld_get_image_name",
    dyld_get_image_vmaddr_slide = "_dyld_get_image_vmaddr_slide", dyld_image_count = "_dyld_image_count",
}
