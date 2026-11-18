//! Core of OmniDoppelganger: the append-only session log, the Anthropic
//! streaming client, the tools, the approval policy, and the agent loop.
//! Nothing here depends on the desktop shell.

pub mod agent;
pub mod anthropic;
pub mod events;
pub mod fsutil;
pub mod llm;
pub mod policy;
pub mod session;
pub mod settings;
pub mod sse;
pub mod tools;
