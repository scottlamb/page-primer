# `page-primer`

`page-primer` speeds up your Rust program's execution by "priming"
memory pages from your binary. It supports two optimizations:

*   `mlock()`ing ELF segments so they are never paged out, avoiding "major page
    fault" stalls while waiting for them to be read back in from SSD or even
    spinning disk.
*   remapping ELF segments to enable huge pages, which can speed up large
    programs by 5–10%. (More below.)

`page-primer` only does anything on Linux at present, though at least the
concept of `mlock()` should apply to any Unix-based system.

## When should you use it?

*   in *applications*, not *libraries*
*   that run on *Linux*
*   and are *long-running*
*   if you care about *CPU efficiency* and/or *latency*
*   and you can afford some *extra RAM*
*   and you can accept the *`unsafe`* blocks

## How do you use it?

1.  Near the top of `main()`, before spawning any threads, add this code:
    ```rust
    let prime_out = page_primer::prime()
        .mlock(true)
        .remap(true) // if desired, see notes.
        .run();
    ```
2.  Further down `main()`, after logging providers have been set up, add this code:
    ```rust
    prime_out.log();
    ```
3. If using remap, add the following to your
   [`.cargo/config.toml`](https://doc.rust-lang.org/cargo/reference/config.html):
   ```toml
   [target.x86_64-unknown-linux-gnu]
   rustflags = [
       "-C", "link-arg=-z",
       "-C", "link-arg=common-page-size=2097152",
       "-C", "link-arg=-z",
       "-C", "link-arg=max-page-size=2097152",
   ]
   ```
4. Verify the performance improvement!

One caveat is that if you later `dlopen` some dynamic library, this code will
not know to prime it.

## Remapping and huge pages

### Background on virtual memory: pages, huge pages, and transpage huge pages

Modern OSs/CPUs use [virtual memory](https://en.wikipedia.org/wiki/Virtual_memory):
memory addresses seen by userspace processes don't directly represent physical
memory locations. Instead, the virtual address space is divided into "pages"
whose meaning is defined by "page tables" maintained by the OS. The CPU's
Memory Mapping Unit (MMU) consults the page tables to translate virtual memory
addresses to physical memory addresses. This mapping helps isolate processes
from each other for security and reliability, among other benefits.

As system RAM sizes have grown to gigabytes and beyond, the page size generally
hasn't changed: it's still 4 KiB on Linux/x86_64. This is a problem! The page
tables have gotten too big for CPUs to consult quickly. While CPUs cache
page tables in a Translation Lookaside Buffer (TLB), they often spend 15% of
their time stalled on TLB cache misses.

The solution is larger pages. Linux/x86_64's still uses 4 KiB pages by default
but also supports 2 MiB or 1 GiB [huge
pages](https://www.kernel.org/doc/html/latest/admin-guide/mm/hugetlbpage.html).
These represent 256 or 262,144 as much RAM for the same TLB space. When they're
used extensively, far less of the CPU's time is spent waiting on TLB misses.

Linux even supports [transparent huge
pages (THP)](https://www.kernel.org/doc/html/latest/admin-guide/mm/transhuge.html)
which are used automatically. But only sometimes. It will not use
transparent huge pages on file-backed mappings unless your kernel was compiled
with the experimental option
[`CONFIG_READ_ONLY_THP_FOR_FS=y`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/mm/Kconfig?h=v6.11-rc4&id=47ac09b91befbb6a235ab620c32af719f8208399#n861).
Most distro kernels are not. That means your program's executable will not take
advantage of huge pages. Unless...

### Remapping for huge pages

Programs can start up, create a new huge page-eligible memory mapping, copy
their code over to it, and remap it over the place of their existing code.
This idea is saner than it sounds and has been around for a while:

*   [libhugetlbfs](https://github.com/libhugetlbfs/libhugetlbfs) has supported
    this idea since 2005. But it uses `hugetlbfs`, which is finicky. The system
    administrator has to arrange for pages to be reserved on startup and a
    special filesystem to be mounted.
*   Google remaps via anonymous `mmap` for many servers running in its
    datacenters and [on ChromeOS](https://chromium.googlesource.com/chromium/src/+/66.0.3359.158/chromeos/hugepage_text/hugepage_text.cc).
*   Facebook remaps via anonymous `mmap` [in HHVM](https://github.com/facebook/hhvm/blob/b3b1562e17f2cedcfbf431f86f492cbdc3988f91/hphp/runtime/base/program-functions.cpp).

`page-primer`'s implementation uses `memfd_create`. Before, `/proc/<pid>/maps`
might look like this:

```text
5646c542d000-5646c55ef000 r--p 00000000 103:03 69612122                  /home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr
5646c562d000-5646c66f3000 r-xp 00200000 103:03 69612122                  /home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr
5646c682d000-5646c6d74000 r--p 01400000 103:03 69612122                  /home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr
5646c7143000-5646c722d000 r--p 01b16000 103:03 69612122                  /home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr
5646c722d000-5646c7230000 rw-p 01c00000 103:03 69612122                  /home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr
...
```

Afterward, it will look like this:

```text
5646c5400000-5646c542d000 r--p 00000000 00:01 25220866                   /memfd:/home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr (deleted)
5646c542d000-5646c55ef000 r--p 0002d000 00:01 25220866                   /memfd:/home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr (deleted)
5646c55ef000-5646c5600000 r--p 001ef000 00:01 25220866                   /memfd:/home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr (deleted)
5646c5600000-5646c562d000 r-xp 00000000 00:01 25220867                   /memfd:/home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr (deleted)
5646c562d000-5646c66f3000 r-xp 0002d000 00:01 25220867                   /memfd:/home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr (deleted)
5646c66f3000-5646c6800000 r-xp 010f3000 00:01 25220867                   /memfd:/home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr (deleted)
5646c6800000-5646c682d000 r--p 00000000 00:01 25220868                   /memfd:/home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr (deleted)
5646c682d000-5646c6d74000 r--p 0002d000 00:01 25220868                   /memfd:/home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr (deleted)
5646c6d74000-5646c6e00000 r--p 00574000 00:01 25220868                   /memfd:/home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr (deleted)
5646c7000000-5646c7143000 rw-p 00000000 00:01 25220869                   /memfd:/home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr (deleted)
5646c7143000-5646c7231000 rw-p 00143000 00:01 25220869                   /memfd:/home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr (deleted)
5646c7231000-5646c7400000 rw-p 00231000 00:01 25220869                   /memfd:/home/slamb/git/moonfire-nvr/server/target/debug/moonfire-nvr (deleted)
```

There is one significant downside: this remapping can break some debugging and
profiling tools' ability to get back traces. Please let me know if you're
aware of an approach that can solve this problem!

### Troubleshooting

Remapping works best if your program's `LOAD` sections are aligned to a
multiple of your platform's transparent huge page size. The `.cargo/config.toml`
snippet above should accomplish this. You can verify it worked via `readelf`:

```text
$ readelf --segments target/debug/examples/simple
...
Program Headers:
  Type           Offset             VirtAddr           PhysAddr
                 FileSiz            MemSiz              Flags  Align
...
  LOAD           0x0000000000000000 0x0000000000000000 0x0000000000000000
                 0x0000000000035308 0x0000000000035308  R      0x200000
  LOAD           0x0000000000200000 0x0000000000200000 0x0000000000200000
                 0x000000000020d391 0x000000000020d391  R E    0x200000
  LOAD           0x0000000000600000 0x0000000000600000 0x0000000000600000
                 0x0000000000079468 0x0000000000079468  R      0x200000
  LOAD           0x00000000007e5d60 0x00000000009e5d60 0x00000000009e5d60
                 0x000000000001a338 0x000000000001a580  RW     0x200000
...
```

Finally, the kernel should actually load the code with alignment that is
a multiple of these boundaries, as verified with `/proc/<pid>/maps`. This should
happen with Linux kernels 5.10 and above. More precisely, your kernel should
have commit
[`ce81bb256a224259ab686742a6284930cbe4f1fa`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/commit/?id=ce81bb256a224259ab686742a6284930cbe4f1fa).
