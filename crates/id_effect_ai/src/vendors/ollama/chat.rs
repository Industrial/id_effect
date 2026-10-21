//! Ollama chat: Ollama serves the OpenAI Chat Completions wire format at
//! `<ollama_base_url>/v1/chat/completions`, so chat goes through the OpenAI adapter (feature
//! `openai`) with [`AiConfig::openai_base_url`](crate::AiConfig::openai_base_url) set to the
//! Ollama URL and any non-empty API key; there is no separate Ollama chat adapter.
