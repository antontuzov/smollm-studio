//! One trait between the agent loop and whatever is answering it.
//!
//! The loop asks for a chat completion with tool schemas attached and gets a
//! stream of events back. Behind that trait: the scripted mock and echo
//! providers every test here runs against, an adapter onto this repository's
//! own inference engine for GGUF models, and a client for anything that speaks
//! the OpenAI wire format — Ollama, LM Studio, vLLM, TGI, `llama-server`, or
//! the `smoll-serve` endpoint this workspace already ships.
//!
//! Small models are the design centre, not an afterthought: tool calls are
//! parsed from native function calls, fenced JSON and plain text in that order,
//! malformed output gets one repair attempt, and a provider that cannot do tool
//! calling is not an error — it degrades the run into suggestion mode.
