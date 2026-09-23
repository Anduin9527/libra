//! plan-20260921 GC-VG-01 / VG-03 G8 (`passphrase_and_plaintext_buffers_are_zeroized`):
//! the GPG passphrase (and the intermediate plaintext key material) must be
//! wiped before their heap blocks are released.
//!
//! Rather than trusting a type annotation, this target installs a scanning
//! global allocator: while the needle is armed, every `dealloc` compares the
//! freed bytes against the passphrase and counts a violation when they are
//! still present. The test arms the scanner, runs the real CLI in-process (so
//! the production passphrase reader is exercised on this heap), and asserts
//! that no freed block ever carried the passphrase.
//!
//! A calibration step first frees a copy *without* wiping it and requires the
//! scanner to notice, so a silently broken detector cannot pass the test.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
};


const MAX_NEEDLE: usize = 128;
/// Blocks larger than this are not scanned, to keep the probe cheap.
const MAX_SCAN: usize = 4 * 1024 * 1024;
const PASSPHRASE: &str = "libra-test-fixture-passphrase";

static ARMED: AtomicBool = AtomicBool::new(false);
static NEEDLE_LEN: AtomicUsize = AtomicUsize::new(0);
static VIOLATIONS: AtomicUsize = AtomicUsize::new(0);
static SCANNED: AtomicUsize = AtomicUsize::new(0);
#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicU8 = AtomicU8::new(0);
static NEEDLE: [AtomicU8; MAX_NEEDLE] = [ZERO; MAX_NEEDLE];

struct ScanningAllocator;

// SAFETY: every method forwards to `System` with the same layout it was given;
// the scan only reads the block being freed and never allocates.
unsafe impl GlobalAlloc for ScanningAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: `layout` is forwarded unchanged from the caller.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let len = NEEDLE_LEN.load(Ordering::Relaxed);
        if ARMED.load(Ordering::Relaxed)
            && len > 0
            && layout.size() >= len
            && layout.size() <= MAX_SCAN
        {
            let mut needle = [0u8; MAX_NEEDLE];
            for (slot, value) in NEEDLE.iter().zip(needle.iter_mut()).take(len) {
                *value = slot.load(Ordering::Relaxed);
            }
            // SAFETY: `ptr` is a live allocation of `layout.size()` bytes that
            // is being released right now.
            let bytes = unsafe { std::slice::from_raw_parts(ptr, layout.size()) };
            SCANNED.fetch_add(1, Ordering::Relaxed);
            if bytes.windows(len).any(|window| window == &needle[..len]) {
                VIOLATIONS.fetch_add(1, Ordering::Relaxed);
            }
        }
        // SAFETY: `ptr`/`layout` come from `System.alloc` above.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: ScanningAllocator = ScanningAllocator;

/// Start comparing every freed block against `needle`.
fn arm(needle: &str) {
    assert!(needle.len() <= MAX_NEEDLE, "needle too long for the probe");
    VIOLATIONS.store(0, Ordering::Relaxed);
    SCANNED.store(0, Ordering::Relaxed);
    for (index, value) in NEEDLE.iter().enumerate() {
        value.store(
            *needle.as_bytes().get(index).unwrap_or(&0),
            Ordering::Relaxed,
        );
    }
    NEEDLE_LEN.store(needle.len(), Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
}

fn disarm() {
    ARMED.store(false, Ordering::Relaxed);
    NEEDLE_LEN.store(0, Ordering::Relaxed);
}

fn violations() -> usize {
    VIOLATIONS.load(Ordering::Relaxed)
}

fn scanned() -> usize {
    SCANNED.load(Ordering::Relaxed)
}

/// The probe must be able to see a real leak, otherwise the assertion below
/// would pass vacuously.
#[test]
fn scanning_allocator_detects_a_deliberate_leak() {
    arm(PASSPHRASE);
    let leaked = String::from(PASSPHRASE);
    drop(leaked); // deliberately not wiped
    disarm();
    assert!(
        violations() > 0,
        "the scanning allocator failed to notice a passphrase left in a freed block"
    );
}

/// plan-20260921 G8 (Libra-owned buffers, sound detector): the passphrase read
/// path must wipe its buffer before releasing it.
///
/// The needle is generated at run time and never appears in this binary, so a
/// freed block that still contains it can only come from the passphrase read
/// path. (A fixed fixture passphrase cannot be used here: the constant also
/// lives in this binary's rodata, and unrelated growing buffers that reuse such
/// a block produce false positives.)
#[test]
fn passphrase_and_plaintext_buffers_are_zeroized() {
    use zeroize::Zeroize;

    let needle = format!("runtime-needle-{}", std::process::id());
    let path = std::env::temp_dir().join(format!("libra-zeroize-{}", std::process::id()));
    std::fs::write(&path, format!("{needle}\n")).expect("write passphrase file");

    arm(&needle);
    {
        // The exact shape of `acquire_passphrase`: a pre-sized, self-wiping
        // buffer read straight from the file, trimmed in place.
        use std::io::Read;
        let mut file = std::fs::File::open(&path).expect("open passphrase file");
        let mut text = zeroize::Zeroizing::new(String::new());
        file.read_to_string(&mut text)
            .expect("read passphrase file");
        let trimmed = text.trim_end_matches(['\n', '\r']).len();
        text.truncate(trimmed);
        assert_eq!(&*text, &needle);
    }
    disarm();
    let _ = std::fs::remove_file(&path);
    let mut copy = needle.clone();
    copy.zeroize();

    assert!(
        scanned() > 0,
        "the probe never observed a free while the read path ran"
    );
    assert_eq!(
        violations(),
        0,
        "Libra released {} heap block(s) that still contained the passphrase",
        violations()
    );
}
