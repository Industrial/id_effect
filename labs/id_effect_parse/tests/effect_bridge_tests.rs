use id_effect::next::{Chunk, Stream};
use id_effect_parse::{
  ParseFailure, ParseStreamError, Parser, parse_stream, parse_text_stream, tag,
};

#[test]
fn parse_stream_collects_chunks() {
  let parser = Parser::new(|input: Vec<u8>| {
    if input == b"abc".to_vec() {
      Ok(("ok".to_string(), Vec::new()))
    } else {
      Err(ParseFailure::new("unexpected bytes"))
    }
  });
  let stream = Stream::<Chunk<u8>>::from_iterable(vec![
    Chunk::from_vec(b"a".to_vec()),
    Chunk::from_vec(b"bc".to_vec()),
  ]);
  let value = parse_stream(parser, stream)
    .block_on()
    .into_result()
    .unwrap();
  assert_eq!(value, "ok");
}

#[test]
fn parse_text_stream_decodes_utf8() {
  let parser = tag("hi");
  let stream = Stream::<Chunk<u8>>::from_iterable(vec![Chunk::from_vec(b"hi".to_vec())]);
  let value = parse_text_stream(parser, stream)
    .block_on()
    .into_result()
    .unwrap();
  assert_eq!(value, "hi");
}

#[test]
fn parse_stream_fails_on_parser_error() {
  let parser = Parser::new(|input: Vec<u8>| {
    if input == b"ok" {
      Ok(("ok".to_string(), Vec::new()))
    } else {
      Err(ParseFailure::new("unexpected bytes"))
    }
  });
  let stream = Stream::<Chunk<u8>>::from_iterable(vec![Chunk::from_vec(b"wrong".to_vec())]);
  let err = parse_stream(parser, stream)
    .block_on()
    .into_result()
    .unwrap_err();
  assert!(matches!(
    err.fail_option(),
    Some(ParseStreamError::Parse(_))
  ));
}

#[test]
fn parse_text_stream_fails_on_invalid_utf8() {
  let parser = tag("hi");
  let stream = Stream::<Chunk<u8>>::from_iterable(vec![Chunk::from_vec(vec![0xff, 0xfe])]);
  let err = parse_text_stream(parser, stream)
    .block_on()
    .into_result()
    .unwrap_err();
  assert!(matches!(err.fail_option(), Some(ParseStreamError::Utf8(_))));
}

#[test]
fn parse_stream_surfaces_upstream_failure() {
  let parser = Parser::new(|input: Vec<u8>| Ok::<_, ParseFailure>((input.len(), Vec::new())));
  let stream = Stream::<Chunk<u8>, &'static str>::fail("boom");
  let err = parse_stream(parser, stream)
    .block_on()
    .into_result()
    .unwrap_err();
  assert_eq!(err.fail_option(), Some(ParseStreamError::Stream("boom")));
}
