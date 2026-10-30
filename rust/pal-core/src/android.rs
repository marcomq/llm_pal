//! Android: cpal needs the JavaVM and a Context (via ndk-context) to list audio devices. A GDExtension is
//! loaded with dlopen, so nothing sets them; fetch them ourselves. Taken from trippy-synth (same author).
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

use jni_sys::{JNI_OK, JNIEnv, JavaVM, jsize};

type GetCreatedJavaVMs = unsafe extern "system" fn(*mut *mut JavaVM, jsize, *mut jsize) -> i32;

const GET_VMS: &CStr = c"JNI_GetCreatedJavaVMs";
const DT_STRTAB: i64 = 5;
const DT_SYMTAB: i64 = 6;
const DT_GNU_HASH: i64 = 0x6fff_fef5;

#[repr(C)]
struct Elf64Dyn {
    tag: i64,
    val: u64,
}

#[repr(C)]
struct Elf64Sym {
    name: u32,
    info: u8,
    other: u8,
    shndx: u16,
    value: u64,
    size: u64,
}

/// `JNI_GetCreatedJavaVMs` from the loaded libart.so. Before Android 12 only libart exports it, and linker
/// namespaces hide libart from `dlopen`/`dlsym`; `dl_iterate_phdr` still lists it, so read its symbol table.
unsafe fn libart_get_vms() -> *mut c_void {
    let mut sym = std::ptr::null_mut::<c_void>();
    libc::dl_iterate_phdr(Some(find_in_libart), (&mut sym as *mut *mut c_void).cast());
    sym
}

unsafe extern "C" fn find_in_libart(info: *mut libc::dl_phdr_info, _: usize, out: *mut c_void) -> c_int {
    let info = &*info;
    if info.dlpi_name.is_null() || !CStr::from_ptr(info.dlpi_name).to_bytes().ends_with(b"/libart.so") {
        return 0;
    }
    let phdrs = std::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize);
    if let Some(dynamic) = phdrs.iter().find(|p| p.p_type == libc::PT_DYNAMIC) {
        let dynamic = (info.dlpi_addr + dynamic.p_vaddr) as *const Elf64Dyn;
        *out.cast::<*mut c_void>() = gnu_hash_lookup(info.dlpi_addr, dynamic, GET_VMS);
    }
    1
}

/// Looks up `name` in a loaded ELF's `.dynsym` through its GNU hash table.
unsafe fn gnu_hash_lookup(base: u64, mut dynamic: *const Elf64Dyn, name: &CStr) -> *mut c_void {
    let (mut strtab, mut symtab, mut hash) = (0, 0, 0);
    while (*dynamic).tag != 0 {
        let d = &*dynamic;
        // Bionic leaves these unrelocated (glibc relocates them in place).
        let addr = if d.val < base { base + d.val } else { d.val };
        match d.tag {
            DT_STRTAB => strtab = addr,
            DT_SYMTAB => symtab = addr,
            DT_GNU_HASH => hash = addr,
            _ => {}
        }
        dynamic = dynamic.add(1);
    }
    if strtab == 0 || symtab == 0 || hash == 0 {
        return std::ptr::null_mut();
    }
    let table = hash as *const u32;
    let (nbuckets, symoffset, bloom_words) = (*table, *table.add(1), *table.add(2) as usize);
    let buckets = table.add(4 + bloom_words * 2);
    let chain = buckets.add(nbuckets as usize);
    let name = name.to_bytes();
    let h = name.iter().fold(5381u32, |h, &c| h.wrapping_mul(33).wrapping_add(c as u32));
    let mut i = *buckets.add((h % nbuckets) as usize);
    if i < symoffset {
        return std::ptr::null_mut();
    }
    loop {
        let ch = *chain.add((i - symoffset) as usize);
        let sym = &*(symtab as *const Elf64Sym).add(i as usize);
        if ch | 1 == h | 1
            && sym.value != 0
            && CStr::from_ptr((strtab + sym.name as u64) as *const c_char).to_bytes() == name
        {
            return (base + sym.value) as *mut c_void;
        }
        if ch & 1 != 0 {
            return std::ptr::null_mut();
        }
        i += 1;
    }
}

static READY: OnceLock<bool> = OnceLock::new();

/// True once the context is set. cpal needs it to open any stream (it panics without), not only to list devices.
pub fn init_context() -> bool {
    *READY.get_or_init(|| {
        let ok = unsafe { try_init() }.is_some();
        if !ok {
            eprintln!("pal: no Android context, using default audio routing");
        }
        ok
    })
}

unsafe fn try_init() -> Option<()> {
    let lib = libc::dlopen(c"libnativehelper.so".as_ptr(), libc::RTLD_NOW);
    let sym = if lib.is_null() { std::ptr::null_mut() } else { libc::dlsym(lib, GET_VMS.as_ptr()) };
    let sym = if sym.is_null() { libart_get_vms() } else { sym };
    if sym.is_null() {
        return None;
    }
    let get_vms: GetCreatedJavaVMs = std::mem::transmute(sym);
    let mut vm: *mut JavaVM = std::ptr::null_mut();
    let mut n: jsize = 0;
    if get_vms(&mut vm, 1, &mut n) != JNI_OK || n < 1 {
        return None;
    }
    let mut env: *mut JNIEnv = std::ptr::null_mut();
    if ((**vm).v1_1.AttachCurrentThread)(vm, (&mut env as *mut *mut JNIEnv).cast(), std::ptr::null_mut()) != JNI_OK {
        return None;
    }
    let f = &(**env).v1_1;
    let cls = (f.FindClass)(env, c"android/app/ActivityThread".as_ptr());
    let mid = if cls.is_null() {
        std::ptr::null_mut()
    } else {
        (f.GetStaticMethodID)(env, cls, c"currentApplication".as_ptr(), c"()Landroid/app/Application;".as_ptr())
    };
    let app =
        if mid.is_null() { std::ptr::null_mut() } else { (f.CallStaticObjectMethodA)(env, cls, mid, std::ptr::null()) };
    if app.is_null() {
        (f.ExceptionClear)(env);
        return None;
    }
    let global = (f.NewGlobalRef)(env, app);
    (f.DeleteLocalRef)(env, app);
    (f.DeleteLocalRef)(env, cls);
    ndk_context::initialize_android_context(vm as *mut c_void, global as *mut c_void);
    Some(())
}
