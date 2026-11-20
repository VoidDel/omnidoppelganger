//! Core of OmniDoppelganger: the append-only session log, the streaming
//! clients for each model provider, the tools, the approval policy, and the
//! agent loop.
//! Nothing here depends on the desktop shell.

pub mod agent;
pub mod anthropic;
pub mod documents;
pub mod events;
pub mod fsutil;
pub mod grants;
pub mod imagegen;
pub mod jobs;
pub mod llm;
pub mod made;
pub mod mcp;
pub mod openai_chat;
mod office;
pub mod openai_responses;
pub mod pdf;
pub mod policy;
pub mod provider_search;
pub mod providers;
pub mod runtime;
pub mod safety;
pub mod sandbox;
pub mod schedule;
pub mod session;
pub mod settings;
pub mod skills;
pub mod sse;
#[cfg(test)]
mod testing;
pub mod tools;
pub mod undo;
pub mod web;
pub mod worktree;
