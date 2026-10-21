//! Ollama (`feature = "ollama"`): a self-hosted model server at
//! [`AiConfig::ollama_base_url`](crate::AiConfig::ollama_base_url). Its embeddings adapter is in
//! `embed` (feature `embeddings`); for chat see [`chat`].
//!
//! Ollama usually listens on a loopback or private address, which the platform client's SSRF
//! policy refuses by default: allow the host explicitly in [`AiConfig::http`](crate::AiConfig::http)
//! (`HttpClientConfig::allow_hosts`), never with the private-network opt-out.

pub mod chat;
#[cfg(feature = "embeddings")]
pub mod embed;
