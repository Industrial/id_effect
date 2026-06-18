use id_effect::{Chunk, Stream, run_blocking};
use id_effect_parse::{ParseFailure, Parser, parse_stream, parse_text_stream, tag};

#[test]
fn parse_stream_collects_chunks() {
  let parser = Parser::new(|input: Vec<u8>| {
    if input == b"abc".to_vec() {
      Ok(("ok".to_string(), Vec::new()))
    } else {
      Err(ParseFailure::new("unexpected bytes"))
    }
  });
  let stream = Stream::from_iterable(vec![
    Chunk::from_vec(b"a".to_vec()),
    Chunk::from_vec(b"bc".to_vec()),
  ]);
  let value = run_blocking(parse_stream(parser, stream), ()).unwrap();
  assert_eq!(value, "ok");
}

#[test]
fn parse_text_stream_decodes_utf8() {
  let parser = tag("hi");
  let stream = Stream::from_iterable(vec![Chunk::from_vec(b"hi".to_vec())]);
  let value = run_blocking(parse_text_stream(parser, stream), ()).unwrap();
  assert_eq!(value, "hi");
}
