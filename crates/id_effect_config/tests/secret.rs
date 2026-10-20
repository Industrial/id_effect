//! `Secret<T>` hygiene (0.11.0 E11.2): values are zeroized on drop (M13) and compared in constant
//! time (L3).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use id_effect_config::{Secret, SecretBytes, SecretString};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// A value that records each `zeroize` call in a shared counter.
struct Recording(Arc<AtomicUsize>);

impl Zeroize for Recording {
  fn zeroize(&mut self) {
    self.0.fetch_add(1, Ordering::SeqCst);
  }
}

fn assert_zeroize_on_drop<T: ZeroizeOnDrop>() {}

#[test]
fn m13_secret_is_zeroized_on_drop() {
  let calls = Arc::new(AtomicUsize::new(0));
  let secret = Secret::new(Recording(Arc::clone(&calls)));
  assert_eq!(calls.load(Ordering::SeqCst), 0, "not before the drop");
  drop(secret);
  assert_eq!(
    calls.load(Ordering::SeqCst),
    1,
    "zeroized exactly once on drop"
  );

  // A value taken out with `into_inner` belongs to the caller and is not zeroized.
  let calls = Arc::new(AtomicUsize::new(0));
  let inner = Secret::new(Recording(Arc::clone(&calls))).into_inner();
  assert_eq!(calls.load(Ordering::SeqCst), 0);
  drop(inner);

  // Each secret zeroizes its own value.
  let calls = Arc::new(AtomicUsize::new(0));
  let a = Secret::new(Recording(Arc::clone(&calls)));
  let b = Secret::new(Recording(Arc::clone(&calls)));
  drop((a, b));
  assert_eq!(calls.load(Ordering::SeqCst), 2);

  assert_zeroize_on_drop::<SecretString>();
  assert_zeroize_on_drop::<SecretBytes>();
}

#[test]
fn l3_secret_eq_uses_constant_time_compare() {
  let s = |v: &str| SecretString::new(v.to_owned());
  assert_eq!(s("hunter2"), s("hunter2"));
  assert_ne!(s("hunter2"), s("hunter3"), "same length, last byte differs");
  assert_ne!(
    s("hunter2"),
    s("Hunter2"),
    "same length, first byte differs"
  );
  assert_ne!(s("hunter2"), s("hunter"), "a prefix is not equal");
  assert_eq!(s(""), s(""));

  let b = |v: &[u8]| SecretBytes::new(v.to_vec());
  assert_eq!(b(&[1, 2, 3]), b(&[1, 2, 3]));
  assert_ne!(b(&[1, 2, 3]), b(&[1, 2, 4]));
  assert_ne!(b(&[1, 2, 3]), b(&[1, 2]));

  let cloned = s("token").clone();
  assert_eq!(cloned, s("token"));
}
