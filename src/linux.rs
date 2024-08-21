// Copyright (C) 2024 Scott Lamb <slamb@slamb.org>
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::Output;
use libc::memfd_create;
use std::ffi::{CStr, OsString};
use std::fmt::Write as _;
use std::io::{Error, ErrorKind};
use std::ops::Range;
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;
use std::str::FromStr;

const HPAGE_PMD_SIZE_PATH: &str = "/sys/kernel/mm/transparent_hugepage/hpage_pmd_size";

/// Turns a page size (which must be a power of 2) into a mask.
fn mask(page_size: usize) -> usize {
    assert!(page_size.is_power_of_two() || page_size > 1);
    page_size - 1
}

/// Returns the platform's base page size.
fn base_page_size() -> usize {
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
    assert_eq!(size.count_ones(), 1); // must be non-zero power of 2.
    size
}

/// Returns the transparent huge page size, if the kernel supports huge pages.
pub(crate) fn huge_page_size() -> Result<Option<usize>, Error> {
    let v = match std::fs::read(HPAGE_PMD_SIZE_PATH) {
        Ok(v) => v,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    Some(parse_huge_page_size(&v)).transpose()
}

fn parse_huge_page_size(data: &[u8]) -> Result<usize, Error> {
    let data = std::str::from_utf8(data).map_err(|e| {
        Error::new(
            ErrorKind::InvalidData,
            format!(
                "unable to parse {} contents {:?} as utf8: {}",
                HPAGE_PMD_SIZE_PATH, data, e
            ),
        )
    })?;
    let size = usize::from_str(data.trim()).map_err(|e| {
        Error::new(
            ErrorKind::InvalidData,
            format!(
                "unable to parse {} contents {:?} as usize: {}",
                HPAGE_PMD_SIZE_PATH, &data, e
            ),
        )
    })?;
    Ok(size)
}

fn program_name() -> OsString {
    std::env::current_exe()
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|_| match OsString::from_str("main") {
            Ok(o) => o,
            Err(_) => unreachable!(),
        })
}

#[cfg(target_pointer_width = "64")]
use libc::Elf64_Word as ElfWord;

#[cfg(target_pointer_width = "32")]
use libc::Elf32_Word as ElfWord;

// ELF protection flags, cast appropriately.
const PF_R: ElfWord = libc::PF_R as ElfWord;
const PF_W: ElfWord = libc::PF_W as ElfWord;
const PF_X: ElfWord = libc::PF_X as ElfWord;

/// Returns a debug string describing the given ELF protection flags.
fn debug_prot(p_flags: ElfWord) -> String {
    let r = if (p_flags & PF_R) != 0 { "r" } else { "-" };
    let w = if (p_flags & PF_R) != 0 { "w" } else { "-" };
    let x = if (p_flags & PF_R) != 0 { "x" } else { "-" };
    format!("{r}{w}{x}")
}

/// Context pointer for `phdr_cb`.
struct Context {
    mlock: bool,
    base_page_mask: usize,

    /// A mask for huge pages, iff huge page remapping should be performed.
    #[cfg(target_os = "linux")]
    huge_page_mask: Option<usize>,

    next_object_i: usize,
    program_name: OsString,
    segments: Vec<Segment>,
}

/// An ELF loadable program segment.
struct Segment {
    /// The object index to which this segment belongs; two `Segment`s come from
    /// the same ELF shared object if they have the same `object_i`.
    object_i: usize,
    flags: ElfWord,

    /// The virtual address range.
    addrs: Range<usize>,

    /// The result of remapping into a huge page.
    #[cfg(target_os = "linux")]
    remap: Option<Result<Range<usize>, HugeError>>,

    /// The result of `mlock`.
    mlock: Option<Result<(), libc::c_int>>,

    /// A NUL-terminated string describing the path to the object.
    path: [u8; libc::PATH_MAX as usize],
}

fn errno() -> i32 {
    unsafe { (*libc::__errno_location()) as i32 }
}

unsafe fn mlock(range: Range<usize>) -> Result<(), libc::c_int> {
    if unsafe { libc::mlock(range.start as *const libc::c_void, range.len()) } == -1 {
        return Err(errno());
    }
    Ok(())
}

/// Callback supplied to `dl_iterate_phdr`.
///
/// This performs the actual operations and records status for later reporting.
///
/// Must not panic due to the FFI boundary.
unsafe extern "C" fn phdr_cb(
    info: *mut libc::dl_phdr_info,
    _size: libc::size_t,
    data: *mut libc::c_void,
) -> libc::c_int {
    if std::panic::catch_unwind(|| unsafe { phdr_cb_inner(&*info, &mut *(data as *mut Context)) })
        .is_err()
    {
        eprintln!("Aborting due to phdr_cb failure.");
        std::process::abort();
    }
    0
}

unsafe fn phdr_cb_inner(info: &libc::dl_phdr_info, ctx: &mut Context) {
    let name = if ctx.next_object_i == 0 {
        ctx.program_name.as_bytes()
    } else {
        unsafe { CStr::from_ptr(info.dlpi_name) }.to_bytes()
    };
    let segs = unsafe { std::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize) };
    for seg in segs {
        if seg.p_type != libc::PT_LOAD {
            continue;
        }
        let vaddr = info.dlpi_addr.wrapping_add(seg.p_vaddr) as usize;
        let vend = vaddr + seg.p_memsz as usize;
        let mut path = [0; libc::PATH_MAX as usize];
        let name_copy_len = std::cmp::min(name.len(), libc::PATH_MAX as usize - 1);
        path[..name_copy_len].copy_from_slice(&name[..name_copy_len]);
        let mut seg = Segment {
            object_i: ctx.next_object_i,
            flags: seg.p_flags,
            addrs: vaddr..vend,
            remap: None,
            mlock: None,
            path,
        };

        #[cfg(target_os = "linux")]
        if let Some(huge_page_mask) = ctx.huge_page_mask {
            seg.remap = Some(unsafe { seg.remap(ctx.base_page_mask, huge_page_mask) });
        }
        if ctx.mlock {
            seg.mlock = Some(unsafe { mlock(seg.addrs.clone()) });
        }

        if ctx.segments.len() < ctx.segments.capacity() {
            ctx.segments.push(seg);
        }
    }
    ctx.next_object_i += 1;
}

fn round_up(addr: usize, mask: usize) -> usize {
    match (addr & mask) != 0 {
        true => (addr & !mask) + mask + 1,
        false => addr,
    }
}

/// Transforms ELF `PF_*` protection flags into `PROT_*` as suitable in `mmap` calls.
fn transform_prot(p_flags: ElfWord) -> libc::c_int {
    let mut out = 0;
    if (p_flags & PF_R) != 0 {
        out |= libc::PROT_READ;
    }
    if (p_flags & PF_W) != 0 {
        out |= libc::PROT_WRITE;
    }
    if (p_flags & PF_X) != 0 {
        out |= libc::PROT_EXEC;
    }
    out
}

pub enum HugeError {
    Unreadable,
    Conflict,
    Writable,
    MemfdCreateFailed(i32),
    FtruncateFailed(i32),
    InitialMmapFailed(i32),
    RemapFailed(i32),
}

impl std::fmt::Display for HugeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HugeError::Unreadable => write!(f, "unreadable"),
            HugeError::Conflict => write!(f, "conflicting mappings within all relevant huge pages"),
            HugeError::Writable => write!(f, "writable"),
            HugeError::MemfdCreateFailed(e) => {
                write!(f, "memfd_create failed: {}", Error::from_raw_os_error(*e))
            }
            HugeError::FtruncateFailed(e) => {
                write!(f, "ftruncate failed: {}", Error::from_raw_os_error(*e))
            }
            HugeError::InitialMmapFailed(e) => {
                write!(f, "initial mmap failed: {}", Error::from_raw_os_error(*e))
            }
            HugeError::RemapFailed(e) => {
                write!(f, "remap failed: {}", Error::from_raw_os_error(*e))
            }
        }
    }
}

/// A reserved virtual address range (one mapped with no permissions).
///
/// See [`Segment::remap`] to understand the purpose of the reservation.
struct Reservation(Range<usize>);

impl Reservation {
    /// Try to reserve an address range. Will return `None`` on overlap with an existing mapping.
    fn new(range: Range<usize>) -> Option<Self> {
        match unsafe {
            libc::mmap(
                range.start as *mut libc::c_void,
                range.len(),
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
                -1,
                0,
            )
        } {
            libc::MAP_FAILED => None,
            r if r == range.start as *mut libc::c_void => Some(Self(range)),
            o => {
                // See mmap(2): "Note that older kernels which do not recognize
                // the MAP_FIXED_NOREPLACE flag will typically (upon detecting a
                // collision with a preexisting mapping) fall back to a
                // "non-MAP_FIXED" type of behavior: they will return an address
                // that is different from the requested address.  Therefore,
                // backward-compatible software should check
                // the returned address against the requested address."
                unsafe {
                    libc::munmap(o, range.len());
                }
                None
            }
        }
    }
}

/// Drops a reservation; note the caller should `std::mem::forget` the reservation to prevent
/// this when the reservation is claimed.
impl Drop for Reservation {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.0.start as *mut libc::c_void, self.0.end - self.0.start);
        }
    }
}

/// Replaces the memory range `map` with a huge page-eligible mapping, copying the subset `copy`.
///
/// SAFETY: the caller must ensure that `map` is not changing during this time.
///
/// 1. the address space is fully claimed by the region to copy or a reservation.
/// 2. there are no other threads running which might unmap this region (and
///    potentially map something else in its place).
/// 3. libc operations (some used here) will not write to this region.
unsafe fn replace(
    path: *const libc::c_char,
    map: Range<usize>,
    copy: Range<usize>,
    flags: ElfWord,
) -> Result<(), HugeError> {
    // copy should be within map.
    debug_assert!(copy.start >= map.start);
    debug_assert!(copy.end <= map.end);

    let fd = memfd_create(path, libc::MFD_CLOEXEC | libc::MFD_HUGETLB);
    if fd == -1 {
        return Err(HugeError::MemfdCreateFailed(errno()));
    }
    if libc::ftruncate(fd, map.len() as i64) == -1 {
        let e = errno();
        libc::close(fd);
        return Err(HugeError::FtruncateFailed(e));
    }
    let tmp_addr = match libc::mmap(
        std::ptr::null_mut(),
        map.len(),
        libc::PROT_READ | libc::PROT_WRITE,
        libc::MAP_SHARED,
        fd,
        0,
    ) {
        libc::MAP_FAILED => {
            let e = errno();
            libc::close(fd);
            return Err(HugeError::InitialMmapFailed(e));
        }
        a => a,
    };
    let dst = copy
        .start
        .wrapping_add(tmp_addr as usize)
        .wrapping_sub(map.start);
    debug_assert!(dst >= tmp_addr as usize);
    debug_assert!(dst + copy.len() <= tmp_addr as usize + map.len());
    libc::memcpy(
        dst as *mut libc::c_void,
        copy.start as *const libc::c_void,
        copy.len(),
    );
    libc::munmap(tmp_addr, map.len());
    if libc::mmap(
        map.start as *mut libc::c_void,
        map.len(),
        transform_prot(flags),
        libc::MAP_PRIVATE | libc::MAP_FIXED,
        fd,
        0,
    ) == libc::MAP_FAILED
    {
        let e = errno();
        libc::close(fd);
        return Err(HugeError::RemapFailed(e));
    }
    libc::close(fd);
    Ok(())
}

impl Segment {
    /// Tries to remap as much of the entry as possible to do soundly.
    ///
    /// This attempts to "reserve" (create a memory mapping that will not
    /// overwrite any existing regions) any portion "before" and "after"
    /// the segment within the same huge page. If either reservation fails,
    /// it will not be able to remap the entire segment into the huge page,
    /// but it will remap the portion that is possible.
    ///
    /// Given a virtual memory pages as follows:
    ///
    /// ```text
    /// huge page: 00001111222233334444
    /// data:      ......ssssssssssss.x
    /// ```
    ///
    /// It's possible to create a mapping for huge pages 1–3 that includes
    /// a bit of padding at the start and most of the segment. The portion
    /// of the segment in huge page 4 can't be remapped because something else
    /// is occupying space in that huge page.
    ///
    /// The result will be as follows:
    ///
    /// ```text
    /// huge page: 00001111222233334444
    /// data:      ....PPSSSSSSSSSSss.x
    /// ```
    ///
    ///
    /// Legend:
    /// ```text
    /// s = this segment (not remapped)
    /// S = this segment (within a remapped page)
    /// P = padding (within a remapped page)
    /// . = unmapped
    /// ```
    pub(crate) unsafe fn remap(
        &mut self,
        base_page_mask: usize,
        huge_page_mask: usize,
    ) -> Result<Range<usize>, HugeError> {
        if (self.flags & PF_R) == 0 {
            // If it's unreadable, it can't be copied. (And would remapping it be useful anyway?)
            return Err(HugeError::Unreadable);
        }
        if (self.flags & PF_W) != 0 {
            // Can't trust that it won't change while we're copying it below.
            return Err(HugeError::Writable);
        }
        let page_range =
            (self.addrs.start & !base_page_mask)..round_up(self.addrs.end, base_page_mask);

        let hugepage_outer_range =
            self.addrs.start & !huge_page_mask..round_up(self.addrs.end, huge_page_mask);
        let hugepage_inner_range =
            round_up(page_range.start, huge_page_mask)..page_range.end & !huge_page_mask;
        let mut start_reservation = None;
        let start = if hugepage_outer_range.start < page_range.start {
            start_reservation = Reservation::new(hugepage_outer_range.start..page_range.start);
            match start_reservation.is_some() {
                true => hugepage_outer_range.start,
                false => hugepage_inner_range.start,
            }
        } else {
            hugepage_inner_range.start
        };
        let mut end_reservation = None;
        let end = if hugepage_outer_range.end > page_range.end {
            end_reservation = Reservation::new(page_range.end..hugepage_outer_range.end);
            match end_reservation.is_some() {
                true => hugepage_outer_range.end,
                false => hugepage_inner_range.end,
            }
        } else {
            hugepage_inner_range.end
        };
        if start >= end {
            return Err(HugeError::Conflict);
        }
        let copy = std::cmp::max(start, page_range.start)..std::cmp::min(end, page_range.end);
        let path = &self.path[0] as *const u8 as *const libc::c_char;
        match replace(path, start..end, copy, self.flags) {
            Ok(()) => {
                std::mem::forget(start_reservation);
                std::mem::forget(end_reservation);
                Ok(start..end)
            }
            Err(e) => Err(e),
        }
    }
}

fn log_maps(when: &'static str, log: &mut Vec<(log::Level, String)>) {
    // `/proc/self/maps`` might be useful for debugging. But take the logged version below with a
    // grain of salt because mappings might change due to the logging's own memory allocations.
    log.push((
        log::Level::Trace,
        match std::fs::read("/proc/self/maps") {
            Ok(maps) => format!("maps {when}:\n{}", String::from_utf8_lossy(&maps[..])),
            Err(e) => format!("couldn't read maps: {e}"),
        },
    ));
}

pub(crate) fn run(options: super::Options) -> Output {
    let mut log = Vec::new();
    log_maps("before", &mut log);

    // This function replaces portions of the memory map referring to program text. It assumes
    // nothing else is changing them, for example by `dlopen(3)` and `dlclose(3)` calls. That
    // assumption can't be verified if there are other threads running.
    match num_threads::num_threads() {
        Some(t) if t.get() == 1 => {}
        Some(t) => {
            log.push((
                log::Level::Warn,
                format!("Skipping page priming: there are {t} threads running; must be 1!"),
            ));
            return Output { log };
        }
        None => {
            log.push((
                log::Level::Warn,
                "Skipping page priming: unable to get thread count!".to_owned(),
            ));
            return Output { log };
        }
    }

    let huge_page_mask = if options.remap {
        match huge_page_size() {
            Ok(Some(s)) => Some(mask(s)),
            Ok(None) => {
                log.push((
                    log::Level::Warn,
                    "Huge page remapping requested but huge pages unavailable.".to_owned(),
                ));
                None
            }
            Err(e) => {
                log.push((
                    log::Level::Warn,
                    format!("Unable to describe huge page size: {e}"),
                ));
                None
            }
        }
    } else {
        None
    };

    if huge_page_mask.is_none() && !options.mlock {
        log.push((
            log::Level::Warn,
            "No page priming operations to perform.".to_owned(),
        ));
        return Output { log };
    }

    let mut ctx = Context {
        mlock: options.mlock,
        base_page_mask: mask(base_page_size()),
        huge_page_mask,
        next_object_i: 0,
        program_name: program_name(),
        segments: Vec::with_capacity(1024),
    };

    // This is where the work actually happens.
    unsafe { libc::dl_iterate_phdr(Some(phdr_cb), &mut ctx as *mut Context as *mut libc::c_void) };

    // Create a nice log message for debugging.
    let mut msg = String::with_capacity(128 * ctx.segments.len());
    msg.push_str("primed pages:\n");
    let mut last_object_i = None;
    for obj in &mut ctx.segments {
        if Some(obj.object_i) != last_object_i {
            let path = CStr::from_bytes_until_nul(&obj.path).expect("path has NUL");
            let _ = writeln!(&mut msg, "object {}:", &path.to_string_lossy());
        }
        let _ = write!(
            &mut msg,
            "* {:012x}-{:012x} {} ->",
            obj.addrs.start,
            obj.addrs.end,
            debug_prot(obj.flags)
        );

        #[cfg(target_os = "linux")]
        match obj.remap.as_ref() {
            Some(Ok(remapped)) => {
                let _ = write!(
                    &mut msg,
                    " remap={:012x}-{:012x}",
                    remapped.start, remapped.end
                );
            }
            Some(Err(e)) => {
                let _ = write!(&mut msg, " remap={}", e);
            }
            None => {}
        }
        match obj.mlock.as_ref() {
            Some(Ok(())) => {
                let _ = write!(&mut msg, " mlock=success");
            }
            Some(Err(e)) => {
                let _ = write!(&mut msg, " mlock={}", Error::from_raw_os_error(*e));
            }
            None => {}
        }
        msg.push('\n');
        last_object_i = Some(obj.object_i);
    }
    log.push((log::Level::Info, msg));
    log_maps("after", &mut log);
    Output { log }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_huge_page_size() {
        assert_eq!(parse_huge_page_size(b"2097152\n").unwrap(), 2097152);
        huge_page_size().unwrap();
    }
}
