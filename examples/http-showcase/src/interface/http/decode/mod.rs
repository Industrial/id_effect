//! Request decoding: the path id and the JSON bodies. A failure is `422 invalid`.

mod decode_json_body;
mod parse_todo_id;

pub use decode_json_body::decode_json_body;
pub use parse_todo_id::parse_todo_id;
