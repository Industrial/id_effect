//! [`Transducer`] — composable list transformation without intermediate collections.
//!
//! Re-exports the single implementation in [`id_effect::next::streaming::transducer`]
//! (deduplicated in 0.6), which also powers `Stream::transduce_items`.

pub use id_effect::next::streaming::transducer::{Reducer, Transducer, filter, map, take};
#[cfg(test)]
mod tests {
  use super::*;

  mod map {
    use super::*;

    #[test]
    fn doubles_values_before_sum() {
      let xf = map(|n: i32| n * 2);
      let sum = xf.transduce([1, 2, 3], Box::new(|acc, n| acc + n), 0);
      assert_eq!(sum, 12);
    }
  }

  mod filter {
    use super::*;

    #[test]
    fn keeps_even_values_before_sum() {
      let xf = filter(|n: &i32| n % 2 == 0);
      let sum = xf.transduce([1, 2, 3, 4], Box::new(|acc, n| acc + n), 0);
      assert_eq!(sum, 6);
    }
  }

  mod take {
    use super::*;

    #[test]
    fn limits_items_before_sum() {
      let xf = take::<i32, i32>(2);
      let sum = xf.transduce([1, 2, 3], Box::new(|acc, n| acc + n), 0);
      assert_eq!(sum, 3);
    }
  }

  mod compose {
    use super::*;

    #[test]
    fn chains_map_and_filter() {
      let xf = map(|n: i32| n + 1).compose(filter(|n: &i32| n % 2 == 0));
      let sum = xf.transduce([1, 2, 3], Box::new(|acc, n| acc + n), 0);
      assert_eq!(sum, 6);
    }
  }
}
