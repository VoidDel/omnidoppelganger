//! Golden Stream Contract Test Suite (`[P1-15]`) covering:
//! - 7 Cloud Provider SSE streams (`anthropic`, `openai_responses`, `gemini`, `deepseek`, `kimi`, `glm`, `openrouter`)
//!   across all 4 native streaming protocols (`Anthropic`, `Responses`, `Gemini`, `Chat`), including thinking/reasoning
//!   signatures, parallel multi-tool calls, prompt-cache usage accounting, keepalive `ping`/comments, and 429 errors;
//! - 6 External Coding-Agent CLI NDJSON streams (`agy`, `codex`, `claude`, `cursor`, `grok`, `opencode`);
//! - Arbitrary byte-boundary chunking (`1..=7` bytes and deterministic pseudo-random splits across multi-byte UTF-8
//!   Chinese and emoji codepoints);
//! - Optional live smoke entrypoint (`ODG_LIVE_SMOKE=1 cargo test -p odg-core --test contracts -- --ignored`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use odg_core::anthropic::{AnthropicClient, StreamAccumulator};
use odg_core::cli_agents::{CliEvent, CliKind, StreamParser, clean_tool_result};
use odg_core::events::{Envelope, Event};
use odg_core::gemini::{GeminiAccumulator, GeminiClient};
use odg_core::llm::{AssistantTurn, Endpoint, Llm, LlmError, LlmRequest, StreamDelta, Tokens};
use odg_core::model_usage::parse_cooldown_from_error;
use odg_core::openai_chat::{ChatAccumulator, ChatClient};
use odg_core::openai_responses::{ResponsesAccumulator, ResponsesClient};
use odg_core::policy::Mode;
use odg_core::providers::{Flavor, tokens as parse_provider_tokens};
use odg_core::session::derive_messages;
use odg_core::sse::SseParser;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc::unbounded_channel;
use tokio_util::sync::CancellationToken;

fn fixture_bytes(rel: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(rel);
    std::fs::read(&path).unwrap_or_else(|err| panic!("failed to read fixture {}: {err}", path.display()))
}

#[derive(Clone, Copy)]
enum ChunkPlan {
    Whole,
    Fixed(usize),
    PseudoRandom(u64),
}

fn split_chunks(bytes: &[u8], plan: ChunkPlan) -> Vec<&[u8]> {
    if bytes.is_empty() {
        return Vec::new();
    }
    match plan {
        ChunkPlan::Whole => vec![bytes],
        ChunkPlan::Fixed(n) => bytes.chunks(n.max(1)).collect(),
        ChunkPlan::PseudoRandom(mut state) => {
            let mut out = Vec::new();
            let mut at = 0usize;
            while at < bytes.len() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let step = ((state % 7) as usize) + 1; // 1..=7 bytes
                let end = (at + step).min(bytes.len());
                out.push(&bytes[at..end]);
                at = end;
            }
            out
        }
    }
}

fn all_chunk_plans() -> Vec<ChunkPlan> {
    vec![
        ChunkPlan::Whole,
        ChunkPlan::Fixed(1),
        ChunkPlan::Fixed(2),
        ChunkPlan::Fixed(3),
        ChunkPlan::Fixed(5),
        ChunkPlan::Fixed(7),
        ChunkPlan::PseudoRandom(0x9e37_79b9_7f4a_7c15),
        ChunkPlan::PseudoRandom(0xd1b5_4a32_d192_ed03),
    ]
}

#[derive(Debug, Clone, PartialEq)]
struct ProviderReplayOutcome {
    turn: AssistantTurn,
    tokens: Option<Tokens>,
    event: Event,
}

#[derive(Clone, Copy)]
enum CloudProtocol {
    Anthropic,
    Responses,
    Gemini(&'static str),
    Chat,
}

fn replay_provider_sse(
    provider_id: &str,
    protocol: CloudProtocol,
    raw: &[u8],
    plan: ChunkPlan,
) -> Result<(ProviderReplayOutcome, Vec<StreamDelta>), LlmError> {
    let mut parser = SseParser::default();
    let mut deltas = Vec::new();
    let mut sse_events = Vec::new();
    for chunk in split_chunks(raw, plan) {
        sse_events.extend(parser.feed(chunk));
    }

    let turn = match protocol {
        CloudProtocol::Anthropic => {
            let mut acc = StreamAccumulator::default();
            for ev in sse_events {
                let trimmed = ev.data.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let val: Value = serde_json::from_str(trimmed)
                    .map_err(|e| LlmError::Protocol(format!("Anthropic JSON error: {e}")))?;
                deltas.extend(acc.apply(&val)?);
            }
            if !acc.stopped() {
                return Err(LlmError::Network("Anthropic stream ended without message_stop".into()));
            }
            acc.finish()
        }
        CloudProtocol::Responses => {
            let mut acc = ResponsesAccumulator::default();
            for ev in sse_events {
                let trimmed = ev.data.trim();
                if trimmed.is_empty() || trimmed == "[DONE]" {
                    continue;
                }
                let val: Value = serde_json::from_str(trimmed)
                    .map_err(|e| LlmError::Protocol(format!("Responses JSON error: {e}")))?;
                deltas.extend(acc.apply(&val)?);
            }
            if !acc.finished() {
                return Err(LlmError::Network("Responses stream ended without completed".into()));
            }
            acc.finish()
        }
        CloudProtocol::Gemini(requested_model) => {
            let mut acc = GeminiAccumulator::default();
            for ev in sse_events {
                let trimmed = ev.data.trim();
                if trimmed.is_empty() || trimmed == "[DONE]" {
                    continue;
                }
                let val: Value =
                    serde_json::from_str(trimmed).map_err(|e| LlmError::Protocol(format!("Gemini JSON error: {e}")))?;
                deltas.extend(acc.apply(&val)?);
            }
            if !acc.finished() {
                return Err(LlmError::Network("Gemini stream ended without finishReason".into()));
            }
            acc.finish(requested_model)?
        }
        CloudProtocol::Chat => {
            let mut acc = ChatAccumulator::default();
            for ev in sse_events {
                let trimmed = ev.data.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed == "[DONE]" {
                    break;
                }
                let val: Value =
                    serde_json::from_str(trimmed).map_err(|e| LlmError::Protocol(format!("Chat JSON error: {e}")))?;
                deltas.extend(acc.apply(&val)?);
            }
            if !acc.finished() {
                return Err(LlmError::Network("Chat stream ended without finish_reason".into()));
            }
            acc.finish()?
        }
    };

    let tokens = parse_provider_tokens(provider_id, &turn.usage);
    let event = Event::AssistantMessage {
        content: turn.content.clone(),
        stop_reason: turn.stop_reason.clone(),
        model: turn.model.clone(),
        usage: turn.usage.clone(),
        tokens,
    };
    Ok((ProviderReplayOutcome { turn, tokens, event }, deltas))
}

fn assert_provider_invariant_across_chunkings(
    provider_id: &str,
    protocol: CloudProtocol,
    fixture_rel: &str,
) -> ProviderReplayOutcome {
    let raw = fixture_bytes(fixture_rel);
    let plans = all_chunk_plans();
    let (baseline, baseline_deltas) =
        replay_provider_sse(provider_id, protocol, &raw, plans[0]).expect("whole-stream replay must succeed");
    assert!(!baseline_deltas.is_empty(), "{fixture_rel} emitted no stream deltas");

    for plan in &plans[1..] {
        let (replayed, deltas) =
            replay_provider_sse(provider_id, protocol, &raw, *plan).expect("chunked replay must succeed");
        assert_eq!(
            replayed, baseline,
            "turn/tokens mismatch under chunking for {fixture_rel}"
        );
        // Concatenated text & thinking deltas must be identical regardless of byte-chunk boundaries!
        let join_thinking = |ds: &[StreamDelta]| -> String {
            ds.iter()
                .filter_map(|d| match d {
                    StreamDelta::ThinkingDelta { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect()
        };
        let join_text = |ds: &[StreamDelta]| -> String {
            ds.iter()
                .filter_map(|d| match d {
                    StreamDelta::TextDelta { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(join_thinking(&deltas), join_thinking(&baseline_deltas));
        assert_eq!(join_text(&deltas), join_text(&baseline_deltas));
    }
    baseline
}

#[test]
fn replays_anthropic_golden_fixture_across_1_to_7_byte_utf8_boundaries() {
    let out = assert_provider_invariant_across_chunkings(
        "anthropic",
        CloudProtocol::Anthropic,
        "providers/anthropic/turn_thinking_parallel_tools.sse",
    );
    assert_eq!(out.turn.model, "claude-opus-5-5");
    assert_eq!(out.turn.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(
        out.tokens,
        Some(Tokens {
            input: 2220,
            cached: 1800,
            cache_write: 0,
            output: 95
        })
    );
    assert_eq!(out.turn.content.len(), 4);
    assert_eq!(
        out.turn.content[0],
        json!({
            "type": "thinking",
            "thinking": "先检查 Cargo.toml 🔍，再并行读取 src/lib.rs。",
            "signature": "sig_anth_9f8e7d6c5b4a"
        })
    );
    assert_eq!(
        out.turn.content[1],
        json!({ "type": "text", "text": "我来并行读取两个核心文件。" })
    );
    assert_eq!(
        out.turn.content[2],
        json!({ "type": "tool_use", "id": "toolu_01A", "name": "read_file", "input": { "path": "Cargo.toml" } })
    );
    assert_eq!(
        out.turn.content[3],
        json!({ "type": "tool_use", "id": "toolu_01B", "name": "read_file", "input": { "path": "src/lib.rs", "limit": 50 } })
    );
}

#[test]
fn replays_openai_responses_golden_fixture_across_1_to_7_byte_utf8_boundaries() {
    let out = assert_provider_invariant_across_chunkings(
        "openai",
        CloudProtocol::Responses,
        "providers/openai_responses/turn_reasoning_parallel_tools.sse",
    );
    assert_eq!(out.turn.model, "gpt-6.2-astra");
    assert_eq!(out.turn.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(
        out.tokens,
        Some(Tokens {
            input: 3400,
            cached: 2048,
            cache_write: 0,
            output: 140
        })
    );
    assert_eq!(out.turn.content.len(), 4);
    assert_eq!(out.turn.content[0]["type"], "thinking");
    assert_eq!(out.turn.content[0]["thinking"], "分析模块依赖关系 ⚙️");
    assert_eq!(
        out.turn.content[0]["item"]["encrypted_content"],
        "enc_sig_openai_xyz123"
    );
    assert_eq!(out.turn.content[1]["text"], "正在并行检索两个文件。");
    assert_eq!(out.turn.content[2]["id"], "call_oa_1");
    assert_eq!(out.turn.content[2]["input"], json!({ "path": "Cargo.toml" }));
    assert_eq!(out.turn.content[3]["id"], "call_oa_2");
    assert_eq!(out.turn.content[3]["input"], json!({ "pattern": "LspHub" }));
}

#[test]
fn replays_gemini_golden_fixture_across_1_to_7_byte_utf8_boundaries() {
    let out = assert_provider_invariant_across_chunkings(
        "gemini",
        CloudProtocol::Gemini("gemini-2.5-pro"),
        "providers/gemini/turn_thought_signature_parallel_tools.sse",
    );
    assert_eq!(out.turn.model, "gemini-2.5-pro");
    assert_eq!(out.turn.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(
        out.tokens,
        Some(Tokens {
            input: 4096,
            cached: 3072,
            cache_write: 0,
            output: 130
        })
    );
    assert_eq!(out.turn.content.len(), 4);
    assert_eq!(
        out.turn.content[0],
        json!({
            "type": "thinking",
            "thinking": "思考：先定位配置与会话模块 🧠",
            "signature": "gem_thought_sig_v1"
        })
    );
    assert_eq!(
        out.turn.content[1],
        json!({ "type": "text", "text": "我将并行调用两个工具。" })
    );
    assert_eq!(out.turn.content[2]["id"], "gem_fc_1");
    assert_eq!(out.turn.content[2]["thought_signature"], "gem_fc_sig_1");
    assert_eq!(out.turn.content[3]["id"], "gem_fc_2");
    assert_eq!(out.turn.content[3]["thought_signature"], "gem_fc_sig_2");
}

#[test]
fn replays_deepseek_kimi_glm_and_openrouter_golden_fixtures_across_1_to_7_byte_boundaries() {
    // 4) DeepSeek
    let ds = assert_provider_invariant_across_chunkings(
        "deepseek",
        CloudProtocol::Chat,
        "providers/deepseek/turn_reasoner_parallel_tools.sse",
    );
    assert_eq!(ds.turn.model, "deepseek-v4-pro");
    assert_eq!(ds.turn.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(
        ds.tokens,
        Some(Tokens {
            input: 2500,
            cached: 1900,
            cache_write: 0,
            output: 110
        })
    );
    assert_eq!(ds.turn.content[0]["thinking"], "逐步推导缓存一致性 🧩");
    assert_eq!(ds.turn.content[2]["id"], "call_ds_0");
    assert_eq!(ds.turn.content[2]["input"], json!({ "path": "Cargo.toml" }));
    assert_eq!(ds.turn.content[3]["id"], "call_ds_1");

    // 5) Kimi (Moonshot) — including `functions.*:0` tool IDs normalized on cross-provider switch!
    let kimi = assert_provider_invariant_across_chunkings(
        "moonshot",
        CloudProtocol::Chat,
        "providers/kimi/turn_thinking_parallel_tools.sse",
    );
    assert_eq!(kimi.turn.model, "kimi-k3.1-pro");
    assert_eq!(
        kimi.tokens,
        Some(Tokens {
            input: 1800,
            cached: 1200,
            cache_write: 0,
            output: 75
        })
    );
    assert_eq!(kimi.turn.content[0]["thinking"], "月之暗面推理链 🌙");
    assert_eq!(kimi.turn.content[2]["id"], "functions.read_file:0");
    assert_eq!(kimi.turn.content[3]["id"], "functions.search:1");

    let envelopes = vec![
        Envelope {
            seq: 1,
            ts: "2026-11-26T00:00:00Z".into(),
            event: Event::SessionCreated {
                id: "s1".into(),
                workspace: "/w".into(),
                provider: Some("moonshot".into()),
                model: "kimi-k3.1-pro".into(),
                format: odg_core::session::FORMAT_VERSION,
                parent: None,
                worktree: None,
                schedule: None,
                fork: None,
                aside: None,
            },
        },
        Envelope {
            seq: 2,
            ts: "2026-11-26T00:00:01Z".into(),
            event: Event::UserMessage {
                text: "go".into(),
                steering: false,
                mode: Mode::Auto,
                images: vec![],
                files: vec![],
            },
        },
        Envelope {
            seq: 3,
            ts: "2026-11-26T00:00:02Z".into(),
            event: kimi.event.clone(),
        },
        Envelope {
            seq: 4,
            ts: "2026-11-26T00:00:03Z".into(),
            event: Event::ToolResult {
                tool_use_id: "functions.read_file:0".into(),
                tool: "read_file".into(),
                content: "ok1".into(),
                is_error: false,
                meta: Value::Null,
                images: vec![],
            },
        },
        Envelope {
            seq: 5,
            ts: "2026-11-26T00:00:04Z".into(),
            event: Event::ToolResult {
                tool_use_id: "functions.search:1".into(),
                tool: "search".into(),
                content: "ok2".into(),
                is_error: false,
                meta: Value::Null,
                images: vec![],
            },
        },
        Envelope {
            seq: 6,
            ts: "2026-11-26T00:00:05Z".into(),
            event: Event::ModelChanged {
                model: "claude-sonnet-5-5".into(),
                provider: Some("anthropic".into()),
            },
        },
    ];
    let derived = derive_messages(&envelopes);
    assert_eq!(derived[1]["content"][1]["id"], "functions_read_file_0");
    assert_eq!(derived[2]["content"][0]["tool_use_id"], "functions_read_file_0");

    // 6) GLM (Zhipu) — including object-valued `arguments`!
    let glm = assert_provider_invariant_across_chunkings(
        "zhipu",
        CloudProtocol::Chat,
        "providers/glm/turn_object_args_parallel_tools.sse",
    );
    assert_eq!(glm.turn.model, "glm-5.3");
    assert_eq!(
        glm.tokens,
        Some(Tokens {
            input: 1600,
            cached: 800,
            cache_write: 0,
            output: 64
        })
    );
    assert_eq!(glm.turn.content[0]["thinking"], "智谱 GLM 思考过程 🔮");
    assert_eq!(glm.turn.content[2]["input"], json!({ "path": "README.md" }));
    assert_eq!(glm.turn.content[3]["input"], json!({ "glob": "*.toml" }));

    // 7) OpenRouter
    let or = assert_provider_invariant_across_chunkings(
        "openrouter",
        CloudProtocol::Chat,
        "providers/openrouter/turn_gateway_parallel_tools.sse",
    );
    assert_eq!(or.turn.model, "anthropic/claude-sonnet-4.6");
    assert_eq!(
        or.tokens,
        Some(Tokens {
            input: 3000,
            cached: 1500,
            cache_write: 0,
            output: 92
        })
    );
    assert_eq!(or.turn.content[0]["thinking"], "跨网关推理校验 🌐");
    assert_eq!(or.turn.content[2]["input"], json!({ "path": "package.json" }));
    assert_eq!(or.turn.content[3]["input"], json!({ "path": "Cargo.toml" }));
}

#[test]
fn replays_rate_limit_429_sse_fixtures_and_extracts_cooldown_deadlines() {
    let now = Utc::now();
    let cases = [
        (
            "anthropic",
            CloudProtocol::Anthropic,
            "providers/anthropic/rate_limit_429.sse",
            45i64,
        ),
        (
            "openai",
            CloudProtocol::Responses,
            "providers/openai_responses/rate_limit_429.sse",
            30i64,
        ),
        (
            "gemini",
            CloudProtocol::Gemini("gemini-2.5-pro"),
            "providers/gemini/rate_limit_429.sse",
            75i64,
        ),
        (
            "openrouter",
            CloudProtocol::Chat,
            "providers/openrouter/rate_limit_429.sse",
            20i64,
        ),
    ];

    for (provider_id, protocol, fixture_rel, expected_secs) in cases {
        let raw = fixture_bytes(fixture_rel);
        for plan in all_chunk_plans() {
            let err = replay_provider_sse(provider_id, protocol, &raw, plan)
                .expect_err("429 SSE stream must surface LlmError::Api");
            assert!(err.retryable(), "{fixture_rel}: expected retryable error, got {err:?}");
            let (until, _reason) =
                parse_cooldown_from_error(&err.to_string(), now).expect("expected cooldown parsed from 429 error");
            assert_eq!((until - now).num_seconds(), expected_secs, "{fixture_rel}");
        }
    }
}

/// Serve a raw HTTP response over a real loopback TCP socket in tiny 5-byte writes to exercise
/// the full async reqwest byte-stream pipeline of all 4 cloud HTTP clients.
async fn serve_dripped_http(status_line: &str, content_type: &str, body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let head = format!(
        "HTTP/1.1 {status_line}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut req_buf = [0u8; 4096];
        let _ = sock.read(&mut req_buf).await;
        sock.write_all(head.as_bytes()).await.unwrap();
        for piece in body.chunks(5) {
            if sock.write_all(piece).await.is_err() {
                break;
            }
            let _ = sock.flush().await;
        }
        let _ = sock.shutdown().await;
    });
    addr
}

#[tokio::test]
async fn end_to_end_http_clients_stream_dripped_golden_fixtures_and_surface_429() {
    let sample_req = |provider: &str, model: &str| LlmRequest {
        provider: provider.into(),
        session: "sess-contract".into(),
        model: model.into(),
        system: "You are a coding assistant.".into(),
        messages: vec![json!({ "role": "user", "content": [{ "type": "text", "text": "inspect" }] })],
        tools: Vec::new(),
        effort: "high".into(),
    };

    // Anthropic live TCP drip
    let anth_url = serve_dripped_http(
        "200 OK",
        "text/event-stream",
        fixture_bytes("providers/anthropic/turn_thinking_parallel_tools.sse"),
    )
    .await;
    let anth = AnthropicClient::new(anth_url, Arc::new(|| Some("sk-test".into()))).unwrap();
    let (tx, _rx) = unbounded_channel();
    let anth_turn = anth
        .stream(sample_req("anthropic", "claude-opus-5-5"), tx, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(anth_turn.content.len(), 4);

    // OpenAI Responses live TCP drip
    let oa_url = serve_dripped_http(
        "200 OK",
        "text/event-stream",
        fixture_bytes("providers/openai_responses/turn_reasoning_parallel_tools.sse"),
    )
    .await;
    let oa = ResponsesClient::new(Arc::new(move |_| Endpoint {
        base_url: oa_url.clone(),
        key: Some("sk-test".into()),
    }))
    .unwrap();
    let (tx, _rx) = unbounded_channel();
    let oa_turn = oa
        .stream(sample_req("openai", "gpt-6.2-astra"), tx, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(oa_turn.content.len(), 4);

    // Gemini live TCP drip
    let gem_url = serve_dripped_http(
        "200 OK",
        "text/event-stream",
        fixture_bytes("providers/gemini/turn_thought_signature_parallel_tools.sse"),
    )
    .await;
    let gem = GeminiClient::new(Arc::new(move |_| Endpoint {
        base_url: gem_url.clone(),
        key: Some("sk-test".into()),
    }))
    .unwrap();
    let (tx, _rx) = unbounded_channel();
    let gem_turn = gem
        .stream(sample_req("gemini", "gemini-2.5-pro"), tx, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(gem_turn.content.len(), 4);

    // ChatClient (DeepSeek flavor) live TCP drip + HTTP 429 non-retryable / fast check
    let ds_url = serve_dripped_http(
        "200 OK",
        "text/event-stream",
        fixture_bytes("providers/deepseek/turn_reasoner_parallel_tools.sse"),
    )
    .await;
    let chat = ChatClient::new(Arc::new(move |_| Endpoint {
        base_url: ds_url.clone(),
        key: Some("sk-test".into()),
    }))
    .unwrap();
    let (tx, _rx) = unbounded_channel();
    let ds_turn = chat
        .stream(
            Flavor::DeepSeek,
            sample_req("deepseek", "deepseek-v4-pro"),
            tx,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(ds_turn.content.len(), 4);
}

#[derive(Debug, Clone, PartialEq)]
struct CliReplaySummary {
    session_id: Option<String>,
    text: String,
    tools: Vec<(String, Value, String, Value, bool)>,
    tokens: Tokens,
    events: Vec<Event>,
}

/// Replays an `.ndjson` byte stream through an arbitrary `1..=7` byte chunk splitter into
/// [`StreamParser`], verifying multi-byte UTF-8 safety and folding normalized [`Event`]s + [`Tokens`].
fn replay_cli_ndjson(kind: CliKind, raw: &[u8], plan: ChunkPlan) -> CliReplaySummary {
    let mut line_buf: Vec<u8> = Vec::new();
    let mut parser = StreamParser::default();
    let mut cli_events: Vec<CliEvent> = Vec::new();

    for chunk in split_chunks(raw, plan) {
        for &byte in chunk {
            if byte == b'\n' {
                if line_buf.last() == Some(&b'\r') {
                    line_buf.pop();
                }
                let line = String::from_utf8(std::mem::take(&mut line_buf))
                    .expect("complete NDJSON line must remain valid UTF-8 across arbitrary byte splits");
                cli_events.extend(parser.parse_line(kind, &line));
            } else {
                line_buf.push(byte);
            }
        }
    }
    if !line_buf.is_empty() {
        if line_buf.last() == Some(&b'\r') {
            line_buf.pop();
        }
        let line = String::from_utf8(line_buf).expect("trailing NDJSON line valid UTF-8");
        cli_events.extend(parser.parse_line(kind, &line));
    }

    let mut session_id: Option<String> = None;
    let mut active_text = String::new();
    let mut full_text = String::new();
    let mut emitted_text = false;
    let mut accounted = Tokens::default();
    let mut pending_tokens: Option<(Value, Tokens)> = None;
    let mut started_tools: std::collections::HashMap<String, (String, Value)> = std::collections::HashMap::new();
    let mut completed_tools: Vec<(String, Value, String, Value, bool)> = Vec::new();
    let mut events: Vec<Event> = Vec::new();

    fn record_conv(backend: &str, conv: Option<String>, session_id: &mut Option<String>, events: &mut Vec<Event>) {
        if let Some(id) = conv
            && !id.is_empty()
            && session_id.as_deref() != Some(id.as_str())
        {
            events.push(Event::ExternalSession {
                backend: backend.to_string(),
                session_id: id.clone(),
            });
            *session_id = Some(id);
        }
    }

    let backend = kind.backend_key();
    for ev in cli_events {
        match ev {
            CliEvent::Init { conversation_id } => record_conv(backend, conversation_id, &mut session_id, &mut events),
            CliEvent::AgentText {
                conversation_id,
                delta,
                done,
                usage,
                tokens,
            } => {
                record_conv(backend, conversation_id, &mut session_id, &mut events);
                if !delta.is_empty() {
                    active_text.push_str(&delta);
                }
                if done {
                    let merged_tokens = match (pending_tokens.take(), tokens) {
                        (None, t) => t,
                        (Some((_, pt)), None) => Some(pt),
                        (Some((_, pt)), Some(t)) => Some(Tokens {
                            input: pt.input + t.input,
                            cached: pt.cached + t.cached,
                            cache_write: 0,
                            output: pt.output + t.output,
                        }),
                    };
                    if !active_text.is_empty() {
                        if let Some(t) = merged_tokens {
                            accounted.input += t.input;
                            accounted.cached += t.cached;
                            accounted.output += t.output;
                        }
                        let body = std::mem::take(&mut active_text);
                        full_text.push_str(&body);
                        emitted_text = true;
                        events.push(Event::AssistantMessage {
                            content: vec![json!({ "type": "text", "text": body })],
                            stop_reason: Some("end_turn".into()),
                            model: "contract-model".into(),
                            usage,
                            tokens: merged_tokens,
                        });
                    } else if let Some(t) = merged_tokens {
                        pending_tokens = Some((usage, t));
                    }
                } else if let Some(t) = tokens {
                    pending_tokens = Some((usage, t));
                }
            }
            CliEvent::ToolStart {
                conversation_id,
                call_key,
                name,
                input,
            } => {
                record_conv(backend, conversation_id, &mut session_id, &mut events);
                let tool_tokens = pending_tokens.take().map(|(_, t)| {
                    accounted.input += t.input;
                    accounted.cached += t.cached;
                    accounted.output += t.output;
                    t
                });
                events.push(Event::AssistantMessage {
                    content: vec![json!({ "type": "tool_use", "id": call_key, "name": name, "input": input })],
                    stop_reason: Some("tool_use".into()),
                    model: "contract-model".into(),
                    usage: Value::Null,
                    tokens: tool_tokens,
                });
                started_tools.insert(call_key, (name, input));
            }
            CliEvent::ToolDone {
                conversation_id,
                call_key,
                name,
                input,
                output,
                is_error,
            } => {
                record_conv(backend, conversation_id, &mut session_id, &mut events);
                let (resolved_name, resolved_input) = started_tools
                    .remove(&call_key)
                    .unwrap_or_else(|| (name.unwrap_or_else(|| "tool".into()), input));
                let (cleaned, meta) = clean_tool_result(&resolved_name, &resolved_input, &output, is_error);
                events.push(Event::ToolResult {
                    tool_use_id: call_key,
                    tool: resolved_name.clone(),
                    content: cleaned.clone(),
                    is_error,
                    meta: meta.clone(),
                    images: Vec::new(),
                });
                completed_tools.push((resolved_name, resolved_input, cleaned, meta, is_error));
            }
            CliEvent::Result {
                conversation_id,
                response,
                usage,
                tokens,
                ..
            } => {
                record_conv(backend, conversation_id, &mut session_id, &mut events);
                let rem = match tokens {
                    Some(total) => {
                        let _ = pending_tokens.take();
                        let r = Tokens {
                            input: total.input.saturating_sub(accounted.input),
                            cached: total.cached.saturating_sub(accounted.cached),
                            cache_write: 0,
                            output: total.output.saturating_sub(accounted.output),
                        };
                        (r.input > 0 || r.cached > 0 || r.output > 0).then_some(r)
                    }
                    None => pending_tokens.take().map(|(_, t)| t),
                };
                if let Some(r) = rem {
                    accounted.input += r.input;
                    accounted.cached += r.cached;
                    accounted.output += r.output;
                }
                if !active_text.is_empty() {
                    let body = std::mem::take(&mut active_text);
                    full_text.push_str(&body);
                    emitted_text = true;
                    events.push(Event::AssistantMessage {
                        content: vec![json!({ "type": "text", "text": body })],
                        stop_reason: Some("end_turn".into()),
                        model: "contract-model".into(),
                        usage,
                        tokens: rem,
                    });
                } else if !emitted_text && !response.trim().is_empty() {
                    full_text.push_str(&response);
                    emitted_text = true;
                    events.push(Event::AssistantMessage {
                        content: vec![json!({ "type": "text", "text": response })],
                        stop_reason: Some("end_turn".into()),
                        model: "contract-model".into(),
                        usage,
                        tokens: rem,
                    });
                } else if let Some(r) = rem {
                    events.push(Event::AssistantMessage {
                        content: Vec::new(),
                        stop_reason: Some("end_turn".into()),
                        model: "contract-model".into(),
                        usage,
                        tokens: Some(r),
                    });
                }
            }
        }
    }
    if let Some((usage, pt)) = pending_tokens.take() {
        accounted.input += pt.input;
        accounted.cached += pt.cached;
        accounted.output += pt.output;
        events.push(Event::AssistantMessage {
            content: Vec::new(),
            stop_reason: Some("end_turn".into()),
            model: "contract-model".into(),
            usage,
            tokens: Some(pt),
        });
    }

    CliReplaySummary {
        session_id,
        text: full_text,
        tools: completed_tools,
        tokens: accounted,
        events,
    }
}

fn assert_cli_invariant_across_chunkings(kind: CliKind, fixture_rel: &str) -> CliReplaySummary {
    let raw = fixture_bytes(fixture_rel);
    let plans = all_chunk_plans();
    let baseline = replay_cli_ndjson(kind, &raw, plans[0]);
    for plan in &plans[1..] {
        let got = replay_cli_ndjson(kind, &raw, *plan);
        assert_eq!(
            got, baseline,
            "CLI NDJSON replay diverged under chunking for {fixture_rel}"
        );
    }
    baseline
}

#[test]
fn replays_all_six_external_cli_golden_ndjson_fixtures_across_1_to_7_byte_utf8_splits() {
    // 1) Antigravity (`agy`)
    let agy = assert_cli_invariant_across_chunkings(CliKind::Antigravity, "cli_agents/agy/session_turn.ndjson");
    assert_eq!(agy.session_id.as_deref(), Some("agy-conv-001"));
    assert_eq!(agy.text, "所有契约测试均已通过 🚀，零警告全绿。");
    assert_eq!(
        agy.tokens,
        Tokens {
            input: 2100,
            cached: 1500,
            cache_write: 0,
            output: 60
        }
    );
    assert_eq!(agy.tools.len(), 2);
    assert_eq!(agy.tools[0].0, "update_todos");
    assert_eq!(agy.tools[0].3["todos"].as_array().unwrap().len(), 2);
    assert_eq!(agy.tools[1].0, "run_command");
    assert_eq!(agy.tools[1].2, "test result: ok.\n");
    assert_eq!(agy.tools[1].3["exit_code"], 0);

    // 2) Codex (`codex`)
    let codex = assert_cli_invariant_across_chunkings(CliKind::Codex, "cli_agents/codex/session_turn.ndjson");
    assert_eq!(codex.session_id.as_deref(), Some("codex-thr-101"));
    assert_eq!(codex.text, "正在修改 session.rs ✅");
    assert_eq!(
        codex.tokens,
        Tokens {
            input: 2400,
            cached: 1800,
            cache_write: 0,
            output: 88
        }
    );
    assert_eq!(codex.tools.len(), 2);
    assert_eq!(codex.tools[0].0, "run_command");
    assert_eq!(codex.tools[1].0, "edit_file");

    // 3) Claude Code (`claude`)
    let claude = assert_cli_invariant_across_chunkings(CliKind::ClaudeCode, "cli_agents/claude/session_turn.ndjson");
    assert_eq!(claude.session_id.as_deref(), Some("claude-ses-202"));
    assert_eq!(claude.text, "已完成审查 🎯。");
    assert_eq!(
        claude.tokens,
        Tokens {
            input: 3150,
            cached: 2600,
            cache_write: 0,
            output: 60
        }
    );
    assert_eq!(claude.tools.len(), 2);
    assert_eq!(claude.tools[0].0, "read_file");
    assert_eq!(claude.tools[1].0, "run_command");

    // 4) Cursor (`cursor-agent`)
    let cursor = assert_cli_invariant_across_chunkings(CliKind::Cursor, "cli_agents/cursor/session_turn.ndjson");
    assert_eq!(cursor.session_id.as_deref(), Some("cursor-chat-303"));
    assert_eq!(cursor.text, "Cursor 契约回放正常 ✨");
    assert_eq!(
        cursor.tokens,
        Tokens {
            input: 3100,
            cached: 2400,
            cache_write: 0,
            output: 60
        }
    );
    assert_eq!(cursor.tools.len(), 1);
    assert_eq!(cursor.tools[0].0, "read_file");
    assert_eq!(cursor.tools[0].3["lines"], 480);

    // 5) Grok (`grok`) — OpenAI-style `prompt_tokens` includes `cached_tokens` without doubling!
    let grok = assert_cli_invariant_across_chunkings(CliKind::Grok, "cli_agents/grok/session_turn.ndjson");
    assert_eq!(grok.session_id.as_deref(), Some("grok-ses-404"));
    assert_eq!(grok.text, "Grok 搜索完毕 ⚡");
    assert_eq!(
        grok.tokens,
        Tokens {
            input: 1500,
            cached: 900,
            cache_write: 0,
            output: 48
        }
    );
    assert_eq!(grok.tools.len(), 1);
    assert_eq!(grok.tools[0].0, "search");

    // 6) OpenCode (`opencode`)
    let opencode = assert_cli_invariant_across_chunkings(CliKind::OpenCode, "cli_agents/opencode/session_turn.ndjson");
    assert_eq!(opencode.session_id.as_deref(), Some("oc-ses-505"));
    assert_eq!(opencode.text, "OpenCode 验证完成 🧩");
    assert_eq!(
        opencode.tokens,
        Tokens {
            input: 2600,
            cached: 2000,
            cache_write: 0,
            output: 44
        }
    );
    assert_eq!(opencode.tools.len(), 1);
    assert_eq!(opencode.tools[0].0, "read_file");
}

/// Live smoke test gated behind `ODG_LIVE_SMOKE=1` (ignored during normal offline CI runs).
#[tokio::test]
#[ignore]
async fn live_smoke_providers_and_cli_agents_when_enabled() {
    if std::env::var("ODG_LIVE_SMOKE").as_deref() != Ok("1") {
        eprintln!("ODG_LIVE_SMOKE!=1; skipping live smoke test.");
        return;
    }

    let request_for = |provider: &str, model: &str| LlmRequest {
        provider: provider.into(),
        session: "live-smoke".into(),
        model: model.into(),
        system: "Reply with the single word PONG.".into(),
        messages: vec![json!({ "role": "user", "content": [{ "type": "text", "text": "ping" }] })],
        tools: Vec::new(),
        effort: "low".into(),
    };

    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY").map(|s| s.trim().to_string())
        && !key.is_empty()
    {
        let client = AnthropicClient::new(
            odg_core::anthropic::DEFAULT_BASE_URL,
            Arc::new(move || Some(key.clone())),
        )
        .unwrap();
        let (tx, _rx) = unbounded_channel();
        let turn = tokio::time::timeout(
            Duration::from_secs(30),
            client.stream(
                request_for("anthropic", "claude-haiku-5-5"),
                tx,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("anthropic live smoke timed out")
        .expect("anthropic live smoke failed");
        assert!(!turn.content.is_empty());
    }

    for kind in [
        CliKind::Antigravity,
        CliKind::Codex,
        CliKind::ClaudeCode,
        CliKind::Cursor,
        CliKind::Grok,
        CliKind::OpenCode,
    ] {
        let found: Option<PathBuf> = kind.find_binary();
        eprintln!("CLI {:?} binary installed: {:?}", kind, found);
    }
}
