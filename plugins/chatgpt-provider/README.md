# chatgpt-provider

NCP plugin: talks to OpenAI's Responses API using ChatGPT-subscription
OAuth credentials. Same multi-instance shape as `openai-provider` (`--name`
flag sets the event-kind prefix), but targets the ChatGPT backend
(`https://chatgpt.com/backend-api/codex`) instead of the standard
`/v1/chat/completions` path.

Includes a standalone OAuth PKCE login flow (`chatgpt-provider login`) that
persists tokens to `$XDG_DATA_HOME/nefor/chatgpt-auth.json`. The plugin
mode (default, no subcommand) runs as an NCP stdio plugin. A running plugin
refreshes OAuth access tokens five minutes before their JWT expiry, serializes
concurrent refreshes, and can adopt a same-account login written by another
process. A Responses 401 is recovered with one credential reload and one
forced refresh before the plugin asks the user to log in again.

The model list is fetched from the backend at runtime -- no `--model` CLI
flag. Users pick via `/model` in the chat surface.

Account quota is read from the ChatGPT usage endpoint at startup, every five
minutes, and on `usage.requested`. Successful Responses headers also refresh
the same snapshot without another request. The starter compositor maps these
native events to `chatgpt/subscription` through conversation-manager's common
usage interface without changing the provider's polling behavior.

## Wire contract

Same chat-scoped event shape as `openai-provider` with `chatgpt` as the
default prefix: `<prefix>.chat.create`, `.chat.append`, `.chat.complete`,
`.chat.delete`, stream events (`<prefix>.stream.delta` / `.stream.end`),
session stats, auth status, model list/status, and lifecycle events. Tool
calling is supported via a `ToolBroker` that consumes `tool.register`, invokes
registered tools, and correlates `tool.result` events back to the in-flight
turn.

Usage adds `<prefix>.usage.requested`, `.usage.updated`, and `.usage.error`.
The update payload carries the backend's primary/secondary windows, reset
timestamps, plan type, and credits without deriving quota from local tokens.

Direct completion accounting publishes the occupancy of the exact lowered
Responses request before it is sent, then replaces that estimate with backend
input usage when the response completes. The local estimate is
`ceil(serialized_request_json_bytes / 4)`: because it runs after provider
lowering, instructions, native/checkpoint items, attachments, tool schemas,
structured output, and reasoning controls each enter through their one wire
representation. Events carry only counts and accuracy, never request content.
Aggregate input/output usage remains separate from current request occupancy.

Provider-hosted web search is selected at process startup with
`--web-search disabled|cached|live` (default: `disabled`). `cached` appends the
native Responses tool `{ "type": "web_search", "external_web_access": false }`;
`live` sets `external_web_access` to `true`; `disabled` omits it. This is a
provider capability, not a Nefor function tool: it is added independently of
local tool registration and per-chat allowlists, including when the local
allowlist is empty. Native search calls remain Responses output items for
provider-context continuation and never enter the local tool gate or invocation
loop.

Direct completions and compaction chats accept an optional closed
`provider_options` object. Its only supported field is
`service_tier: "fast"`, which maps to `service_tier: "priority"` on Responses requests,
matching Codex's ChatGPT subscription transport;
omission (or an empty object) keeps the standard service tier. ChatGPT owns
this validation and rejects unknown fields or tier values before HTTP.

## Stream recovery

A Responses attempt is provisional until the provider emits a semantic terminal
event. Transient connection resets, premature EOFs, idle timeouts, and transient
provider failures therefore discard the entire attempt and replay the same
provider round even when text, reasoning, native output, or tool-call fragments
have already streamed. Discarded content remains available in the canonical
conversation audit, but is retracted from the TUI and excluded from future model
context. Tool calls are delivered only after a successful terminal event, so an
abandoned attempt cannot have executed a local tool.

Recovery continues until success or explicit cancellation by default. Backoff
uses full jitter with a 30-second ceiling, and one provider-process recovery gate
admits a single half-open probe after a shared outage so concurrent agents do not
retry in lockstep. Set `--stream-retry-timeout-seconds <seconds>` only when an
embedding needs a finite elapsed recovery window.

Image media returned by tools such as `read_image` is converted to Responses
API `InputImage` items for vision-capable models. If the active model cannot
accept images, the provider returns an explicit model-capability error instead
of silently dropping the media.

Responses output items are preserved as opaque, provider/model-scoped context
on their canonical assistant messages. Completed items are ordered by
`output_index`; missing indices follow indexed items in stable arrival order,
while duplicate indices fail the turn. Compatible continuations replay the
original encrypted reasoning, message, web-search, and function-call items
before tool results; retained web-search IDs stay in provider context but are
omitted from normal non-stored ChatGPT request input, matching Codex lowering.
Session replay reconstructs the same context. Public conversation and display
projections keep only the provider-neutral message and never expose the
encrypted artifact.

## Updating model compatibility

When adding support for newly released models, refresh the authoritative Codex
checkout and inspect the newest `rust-v*` release tag. Codex derives the
`client_version` sent to its model catalog from the whole major, minor, and
patch version in `codex-rs/Cargo.toml`; verify that path through
`codex-rs/models-manager/src/lib.rs` and `manager.rs`, then copy the resulting
version to `responses::CODEX_COMPAT_CLIENT_VERSION`. A stale value can hide
catalog models whose `minimal_client_version` gate is newer. Run
`just test-provider` from the repository root after updating it.

## Run

Spawned by the engine over stdio. Use `chatgpt-provider login` first to
bootstrap OAuth credentials, then spawn normally in `init.lua`.
