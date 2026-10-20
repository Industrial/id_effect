//! Protocol constants shared by the server and the clients. The message types are in
//! [`crate::wire`] (the labs `{kind, tag, success | failure}` format is gone since 0.12).

/// Default path of the RPC routes (`POST /rpc`, `POST /rpc/{tag}`, `GET /rpc/{tag}`).
pub const RPC_DISPATCH_PATH: &str = "/rpc";
