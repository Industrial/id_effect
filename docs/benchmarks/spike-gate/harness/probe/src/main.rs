//! Referee probes: `allocs`, `lazy`, `stack <v06|a|b> <async|sync> <N> [stack_kib]`.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use spike_gate_probe::{a, b, v06};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
  unsafe fn alloc(&self, l: Layout) -> *mut u8 {
    ALLOCS.fetch_add(1, Ordering::Relaxed);
    BYTES.fetch_add(l.size(), Ordering::Relaxed);
    unsafe { System.alloc(l) }
  }
  unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
    unsafe { System.dealloc(p, l) }
  }
  unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
    ALLOCS.fetch_add(1, Ordering::Relaxed);
    BYTES.fetch_add(n, Ordering::Relaxed);
    unsafe { System.realloc(p, l, n) }
  }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn count(f: impl Fn() -> usize) -> (usize, usize) {
  f(); // warm up thread-locals / global runtime
  let (a0, b0) = (ALLOCS.load(Ordering::SeqCst), BYTES.load(Ordering::SeqCst));
  std::hint::black_box(f());
  (
    ALLOCS.load(Ordering::SeqCst) - a0,
    BYTES.load(Ordering::SeqCst) - b0,
  )
}

fn run(imp: &str, kind: &str, n: usize) -> usize {
  match (imp, kind) {
    ("v06", "async") => v06::async_chain(n),
    ("v06", "sync") => v06::sync_chain(n),
    ("a", "async") => a::async_chain(n),
    ("a", "sync") => a::sync_chain(n),
    ("b", "async") => b::async_chain(n),
    ("b", "sync") => b::sync_chain(n),
    _ => panic!("bad impl/kind"),
  }
}

fn main() {
  let args: Vec<String> = std::env::args().collect();
  match args.get(1).map(String::as_str) {
    Some("allocs") => {
      println!("impl kind n allocs bytes");
      for imp in ["v06", "a", "b"] {
        for kind in ["async", "sync"] {
          for n in [1usize, 16, 64, 256] {
            let (c, by) = count(|| run(imp, kind, n));
            println!("{imp} {kind} {n} {c} {by}");
          }
        }
      }
    }
    Some("lazy") => {
      println!("impl flat_map_ran_at_build map_ran_at_build");
      let (f, g) = v06::laziness();
      println!("v06 {f} {g}");
      let (f, g) = a::laziness();
      println!("a {f} {g}");
      let (f, g) = b::laziness();
      println!("b {f} {g}");
    }
    Some("stack") => {
      let imp = args[2].clone();
      let kind = args[3].clone();
      let n: usize = args[4].parse().unwrap();
      let kib: usize = args.get(5).map_or(2048, |s| s.parse().unwrap());
      let h = std::thread::Builder::new()
        .stack_size(kib * 1024)
        .spawn(move || run(&imp, &kind, n))
        .unwrap();
      let out = h.join().expect("thread panicked");
      assert_eq!(out, n);
      println!("ok {}", out);
    }
    Some("latency-a") => {
      // fork(sleep 10 s) on A's tokio executor, let it suspend, then interrupt_now -> exit observed.
      use std::time::{Duration, Instant};
      let rt = kernel_a::Runtime::global();
      let mut samples = Vec::new();
      for _ in 0..200 {
        let h = rt.fork(kernel_a::sleep::<(), kernel_a::HNil>(Duration::from_secs(
          10,
        )));
        std::thread::sleep(Duration::from_micros(500));
        let t = Instant::now();
        h.interrupt_now();
        let exit = rt.block_on_future(h.join_future()).expect("edge");
        assert!(exit.is_err());
        samples.push(t.elapsed());
      }
      samples.sort();
      println!(
        "a interrupt latency (tokio executor, 200 samples) p50={:?} p99={:?}",
        samples[100], samples[198]
      );
    }
    _ => eprintln!("usage: probe allocs|lazy|stack IMPL KIND N [KIB] | latency-a"),
  }
}
