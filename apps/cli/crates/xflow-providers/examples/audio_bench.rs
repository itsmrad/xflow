//! Synthetic WAV encoding probe; no microphone, network or credential access.
//! Run: cargo run --release -p xflow-providers --example audio_bench
//! CSV: rate,channels,seconds,median_ns,allocated_bytes,allocation_calls,wav_hash.
//! Allocated bytes sum allocation/reallocation requests; they are not peak RSS.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};
use xflow_core::AudioClip;
struct Count;
static BYTES: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Count {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        BYTES.fetch_add(l.size(), Ordering::Relaxed);
        CALLS.fetch_add(1, Ordering::Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        BYTES.fetch_add(n, Ordering::Relaxed);
        CALLS.fetch_add(1, Ordering::Relaxed);
        System.realloc(p, l, n)
    }
}
#[global_allocator]
static ALLOC: Count = Count;
fn main() {
    for (rate, channels) in [(16000, 1), (48000, 1), (48000, 2)] {
        for seconds in [5, 30, 120] {
            let audio = AudioClip {
                sample_rate: rate,
                channels,
                samples: (0..rate as usize * seconds * channels as usize)
                    .map(|i| (i as f32 * 0.031).sin() * 0.5)
                    .collect(),
            };
            let reference = xflow_providers::encode_wav(&audio).unwrap();
            let hash = reference
                .iter()
                .fold(0u64, |h, &b| h.wrapping_mul(31).wrapping_add(b as u64));
            let mut times = vec![];
            let mut allocations = 0;
            let mut bytes = 0;
            for _ in 0..31 {
                BYTES.store(0, Ordering::Relaxed);
                CALLS.store(0, Ordering::Relaxed);
                let start = Instant::now();
                let out = black_box(xflow_providers::encode_wav(black_box(&audio)).unwrap());
                let elapsed = start.elapsed().as_nanos();
                bytes = BYTES.load(Ordering::Relaxed);
                allocations = CALLS.load(Ordering::Relaxed);
                assert_eq!(out, reference);
                times.push(elapsed);
            }
            times.sort();
            println!(
                "{rate},{channels},{seconds},{},{bytes},{allocations},{hash}",
                times[15]
            );
        }
    }
}
