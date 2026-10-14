use id_effect_parse::Doc;

#[test]
fn break_respects_group_width() {
  let doc = Doc::group(
    Doc::text("hello")
      .cat(Doc::break_())
      .cat(Doc::text("world")),
  );
  let flat = doc.render(80);
  assert_eq!(flat, "hello world");
  let broken = doc.render(8);
  assert!(broken.contains('\n'));
}

#[test]
fn fill_joins_with_soft_breaks() {
  let doc = Doc::fill(vec![Doc::text("a"), Doc::text("b"), Doc::text("c")]);
  assert_eq!(doc.render(80), "a b c");
}
