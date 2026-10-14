use id_effect_parse::{Diff, apply_diff, diff_option, diff_values};

#[test]
fn diff_values_detects_change() {
  assert!(matches!(diff_values(1, 1), Diff::Unchanged(1)));
  assert!(matches!(diff_values(1, 2), Diff::Changed { .. }));
}

#[test]
fn diff_option_tracks_insert_remove() {
  assert!(matches!(diff_option(None, Some(1)), Some(Diff::Added(1))));
  assert!(matches!(diff_option(Some(1), None), Some(Diff::Removed(1))));
}

#[test]
fn apply_diff_replaces_value() {
  let next = apply_diff(Some(1), &Diff::Changed { old: 1, new: 2 });
  assert_eq!(next, Some(2));
}

#[test]
fn diff_option_none_none_is_none() {
  assert!(diff_option(None::<i32>, None).is_none());
}

#[test]
fn diff_option_unchanged_when_equal() {
  assert!(matches!(
    diff_option(Some(1), Some(1)),
    Some(Diff::Unchanged(1))
  ));
}

#[test]
fn apply_diff_removed_falls_back_to_base() {
  assert_eq!(apply_diff(Some(1), &Diff::Removed(1)), Some(1));
}

#[test]
fn diff_is_unchanged_flag() {
  assert!(Diff::Unchanged(1).is_unchanged());
  assert!(!Diff::Changed { old: 1, new: 2 }.is_unchanged());
}
