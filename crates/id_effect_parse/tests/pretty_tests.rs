use id_effect_parse::{Doc, Pretty};

#[test]
fn doc_concatenates_text() {
  let doc = Doc::text("hello").cat(Doc::text(" world"));
  assert_eq!(doc.render(80), "hello world");
}

#[test]
fn pretty_trait_formats_primitives() {
  assert_eq!(42i64.pretty().render(80), "42");
  assert_eq!("hi".pretty().render(80), "hi");
}

#[test]
fn pretty_formats_slices() {
  let doc = [1i64, 2, 3].pretty();
  assert_eq!(doc.render(80), "[1, 2, 3]");
}

#[test]
fn group_breaks_when_too_wide() {
  let doc = Doc::text("abcdef")
    .cat(Doc::break_())
    .cat(Doc::text("ghij"))
    .group();
  assert!(doc.render(5).contains(' '));
}
