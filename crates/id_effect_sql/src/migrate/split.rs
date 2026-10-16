//! Splits a migration script into statements: drivers prepare one statement at a time.
//!
//! A `;` ends a statement unless it is inside a string (`'..'`, `E'..'` with backslash escapes),
//! a quoted identifier (`".."`), a dollar-quoted body (`$$..$$`, `$tag$..$tag$`) or a comment
//! (`-- ..`, nested `/* .. */`). Comments are dropped; statements that are empty after that are
//! skipped.

/// The statements of `script`, trimmed, without comments, in order.
pub(crate) fn statements(script: &str) -> Vec<String> {
  let chars: Vec<char> = script.chars().collect();
  let mut out = Vec::new();
  let mut current = String::new();
  let mut i = 0;
  while i < chars.len() {
    let next = chars.get(i + 1).copied();
    match chars[i] {
      '-' if next == Some('-') => {
        while i < chars.len() && chars[i] != '\n' {
          i += 1;
        }
        current.push(' ');
        continue;
      }
      '/' if next == Some('*') => {
        i = block_comment_end(&chars, i);
        current.push(' ');
        continue;
      }
      '\'' => {
        let end = quoted_end(&chars, i, '\'', escape_string(&current));
        current.extend(&chars[i..end]);
        i = end;
        continue;
      }
      '"' => {
        let end = quoted_end(&chars, i, '"', false);
        current.extend(&chars[i..end]);
        i = end;
        continue;
      }
      '$' => {
        if let Some(end) = dollar_quoted_end(&chars, i, &current) {
          current.extend(&chars[i..end]);
          i = end;
          continue;
        }
        current.push('$');
      }
      ';' => flush(&mut current, &mut out),
      c => current.push(c),
    }
    i += 1;
  }
  flush(&mut current, &mut out);
  out
}

fn flush(current: &mut String, out: &mut Vec<String>) {
  let stmt = current.trim();
  if !stmt.is_empty() {
    out.push(stmt.to_owned());
  }
  current.clear();
}

fn is_ident(c: char) -> bool {
  c.is_alphanumeric() || c == '_' || c == '$'
}

/// Whether a `'` after `before` opens an escape string (`E'..'`, backslash escapes).
fn escape_string(before: &str) -> bool {
  let mut rev = before.chars().rev();
  matches!(rev.next(), Some('E' | 'e')) && !rev.next().is_some_and(is_ident)
}

/// The index after the `*/` closing the (nested) block comment opened at `start`.
fn block_comment_end(chars: &[char], start: usize) -> usize {
  let (mut depth, mut i) = (0usize, start);
  while i < chars.len() {
    match (chars[i], chars.get(i + 1).copied()) {
      ('/', Some('*')) => {
        depth += 1;
        i += 2;
      }
      ('*', Some('/')) => {
        depth -= 1;
        i += 2;
        if depth == 0 {
          return i;
        }
      }
      _ => i += 1,
    }
  }
  chars.len()
}

/// The index after the `quote` closing the quoted text opened at `start` (a doubled quote is an
/// escaped quote; with `backslash`, `\` escapes the next character).
fn quoted_end(chars: &[char], start: usize, quote: char, backslash: bool) -> usize {
  let mut i = start + 1;
  while i < chars.len() {
    let c = chars[i];
    let escaped = (backslash && c == '\\') || (c == quote && chars.get(i + 1) == Some(&quote));
    if escaped {
      i += 2;
    } else if c == quote {
      return i + 1;
    } else {
      i += 1;
    }
  }
  chars.len()
}

/// When a dollar quote (`$$` or `$tag$`) opens at `start`, the index after its closing tag.
/// `$1` (a parameter) and `a$b` (an identifier) are not dollar quotes.
fn dollar_quoted_end(chars: &[char], start: usize, before: &str) -> Option<usize> {
  if before.chars().next_back().is_some_and(is_ident) {
    return None;
  }
  let mut j = start + 1;
  while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
    j += 1;
  }
  if chars.get(j) != Some(&'$') || chars.get(start + 1).is_some_and(|c| c.is_ascii_digit()) {
    return None;
  }
  let tag = &chars[start..=j];
  let body = j + 1;
  let close = (body..chars.len()).find(|&k| chars[k..].starts_with(tag));
  Some(close.map_or(chars.len(), |k| k + tag.len()))
}

#[cfg(test)]
mod tests {
  use super::statements;

  #[test]
  fn splits_on_top_level_semicolons_and_drops_comments() {
    assert_eq!(
      statements("-- header\nCREATE TABLE t (id INT); -- trailing\n\nINSERT INTO t VALUES (1);;"),
      ["CREATE TABLE t (id INT)", "INSERT INTO t VALUES (1)"]
    );
    assert_eq!(statements("SELECT 1"), ["SELECT 1"]);
    assert!(statements("  -- only a comment\n /* and; a block */ ;").is_empty());
    assert_eq!(
      statements("SELECT 1 /* a /* nested; */ comment */ + 2"),
      ["SELECT 1   + 2"]
    );
  }

  #[test]
  fn keeps_semicolons_inside_strings_and_identifiers() {
    assert_eq!(
      statements("INSERT INTO t VALUES ('a;b', 'it''s; ok'); SELECT \"we;ird\"\"col\" FROM t"),
      [
        "INSERT INTO t VALUES ('a;b', 'it''s; ok')",
        "SELECT \"we;ird\"\"col\" FROM t"
      ]
    );
    assert_eq!(
      statements(r"SELECT E'a\'; b', e'\\'; SELECT 'x\'; SELECT 2"),
      [r"SELECT E'a\'; b', e'\\'", r"SELECT 'x\'", "SELECT 2"]
    );
    assert_eq!(
      statements("SELECT 'not -- a comment', '/* nor this */'"),
      ["SELECT 'not -- a comment', '/* nor this */'"]
    );
  }

  #[test]
  fn keeps_dollar_quoted_bodies_whole() {
    let body = "CREATE FUNCTION f() RETURNS INT AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql";
    assert_eq!(
      statements(&format!("{body}; SELECT f()")),
      [body, "SELECT f()"]
    );
    let tagged = "DO $body$ BEGIN PERFORM 1; RAISE NOTICE '$$'; END $body$";
    assert_eq!(
      statements(&format!("{tagged};SELECT 1")),
      [tagged, "SELECT 1"]
    );
  }

  #[test]
  fn parameters_and_identifiers_with_dollars_are_not_quotes() {
    assert_eq!(
      statements("SELECT $1; SELECT a$b$ FROM t; SELECT $ ; SELECT $x"),
      ["SELECT $1", "SELECT a$b$ FROM t", "SELECT $", "SELECT $x"]
    );
  }

  #[test]
  fn unterminated_quotes_and_comments_run_to_the_end() {
    assert_eq!(statements("SELECT 'open; still"), ["SELECT 'open; still"]);
    assert_eq!(statements("SELECT \"open; still"), ["SELECT \"open; still"]);
    assert_eq!(
      statements("SELECT $$ open; still"),
      ["SELECT $$ open; still"]
    );
    assert_eq!(statements("SELECT 1 /* open; still"), ["SELECT 1"]);
    assert_eq!(statements("SELECT E'\\"), ["SELECT E'\\"]);
  }
}
