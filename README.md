# comp2resp

`comp2resp` is a Rust HTTP proxy that accepts an OpenAI-compatible `POST /v1/chat/completions` request, translates it into an upstream OpenAI `POST /v1/responses` request, and returns a chat-completions-compatible response.

The project is intentionally strict. Unsupported or ambiguous request shapes are rejected with explicit machine-readable errors instead of being silently ignored, partially translated, or coerced into a best-effort request.

## Status

This project is being built spec-first. The implementation follows this README contract.

## Goals

- Accept OpenAI-compatible chat completion requests.
- Translate them into OpenAI Responses API requests.
- Return chat-completions-compatible responses.
- Support both non-streaming and streaming requests.
- Preserve correctness over broad compatibility.
- Emit explicit errors for invalid or unsupported behavior.

## Non-Goals

- Acting as a general OpenAI API reverse proxy.
- Supporting multiple upstream providers in the first version.
- Silently accepting loosely compatible or partially mappable request fields.
- Preserving undocumented behavior from third-party OpenAI-compatible providers.
- Supporting every historical chat-completions field in the first release.

## Supported Surface

### Inbound endpoint

- `POST /v1/chat/completions`

### Operational endpoints

- `GET /healthz`
- `GET /readyz`

### Upstream endpoint

- `POST {OPENAI_BASE_URL}/v1/responses`

The first version targets OpenAI Responses semantics only.

## API Contract

### Accepted request content type

- `Content-Type: application/json`

Any other content type returns `415 unsupported_media_type`.

### Authentication

Inbound authentication is optional and proxy-configurable.

Supported modes:

- no inbound auth
- static bearer token validation
- pass-through bearer token forwarding

Upstream authentication is always bearer-token based.

### Required request fields

- `model`
- `messages`

### Supported request fields in v1

- `model`
- `messages`
- `stream`
- `temperature`
- `top_p`
- `max_tokens`
- `max_completion_tokens`
- `tools`
- `tool_choice`
- `user`
- `metadata`

### Rejected request fields in v1

The proxy rejects these with `422 unsupported_feature` because they either do not map safely to the Responses API contract used here or require additional compatibility semantics not implemented in v1:

- `n` when present and not equal to `1`
- `logprobs`
- `top_logprobs`
- `logit_bias`
- `presence_penalty`
- `frequency_penalty`
- `seed`
- `response_format`
- `parallel_tool_calls`
- `functions`
- `function_call`
- `audio`
- `modalities`
- `prediction`
- `service_tier`
- `store`
- `reasoning_effort`
- any unknown top-level field

If a field is unsupported, the request fails. The proxy does not drop it and continue.

## Message Compatibility

### Supported message roles

- `system`
- `user`
- `assistant`
- `tool`

### Supported message content forms

#### `system`

- string content only

#### `user`

- string content
- array content containing only text parts

#### `assistant`

- string content
- explicit `tool_calls`

#### `tool`

- string content with `tool_call_id`

### Rejected message shapes in v1

- image parts
- audio parts
- file parts
- refusal parts supplied by the caller
- assistant messages containing both incompatible content structures and tool calls
- messages with unknown roles
- messages missing required role-specific fields

## Translation Rules

### Request translation: chat completions -> responses

The proxy converts the chat-completions request into a Responses request using these rules.

### Model

- `model` is forwarded as-is.

### Stream

- `stream: true` becomes upstream `stream: true`.
- `stream: false` or omitted becomes upstream `stream: false`.

### Input construction

Inbound `messages` become Responses `input` items.

Mapping rules:

- `system` message -> response input item with role `system` and text content
- `user` message -> response input item with role `user` and text content
- `assistant` message with plain text -> response input item with role `assistant` and output text content
- `assistant` message with `tool_calls` -> response input item with role `assistant` and function call items
- `tool` message -> function call output item tied to `tool_call_id`

### Token limit fields

- If `max_completion_tokens` is present, it is used.
- Else if `max_tokens` is present, it is used.
- If both are present and differ, the request is rejected with `422 conflicting_parameters`.

The selected value maps to upstream output token limiting.

### Sampling fields

- `temperature` is forwarded.
- `top_p` is forwarded.

Validation rules:

- `temperature` must be finite.
- `top_p` must be finite and within supported bounds.

### Tools

- Chat-completions `tools` of type `function` map to Responses tools of type `function`.
- Tool schema is preserved.
- Unsupported tool types are rejected.

### Tool choice

- `none`, `auto`, and named function selection are supported when representable upstream.
- Unsupported tool choice shapes are rejected.

### User field

- `user` is forwarded as metadata only if configured to preserve it.
- If user forwarding is disabled, the field is rejected rather than silently dropped.

### Metadata

- `metadata` is forwarded when valid for upstream.
- Invalid metadata values are rejected.

## Response Translation

### Non-streaming responses

The proxy converts a completed Responses API object into a single chat completion response object.

Returned top-level fields:

- `id`
- `object = "chat.completion"`
- `created`
- `model`
- `choices`
- `usage`
- `system_fingerprint = null`

### Choice semantics

The first version returns exactly one choice.

- `choices[0].index = 0`
- `choices[0].message.role = "assistant"`
- `choices[0].message.content` is synthesized from upstream output text
- `choices[0].message.tool_calls` is synthesized from upstream function call output items when present
- `choices[0].finish_reason` is derived from upstream completion state

### Finish reason mapping

Supported finish reasons:

- `stop`
- `length`
- `tool_calls`
- `content_filter`

If the upstream response cannot be mapped to one of these safely, the proxy fails with `502 upstream_translation_failed` instead of guessing.

### Usage mapping

When usage is present upstream, the proxy maps it into chat-completions usage fields.

If usage is absent or structurally incompatible, the proxy returns `usage: null` only if that behavior is valid for the response shape being emitted. Otherwise it fails explicitly.

## Streaming Contract

### Inbound

If the caller sends `stream: true`, the proxy opens an SSE request to the upstream Responses API and emits chat-completions-style SSE chunks.

### Outbound content type

- `Content-Type: text/event-stream`
- `Cache-Control: no-cache`
- `Connection: keep-alive`

### Event translation

Responses semantic events are translated into chat-completions data-only SSE frames.

Streaming behavior:

- emit an initial assistant role delta chunk
- emit content deltas as chat completion `choices[0].delta.content`
- emit tool call deltas when upstream produces function call arguments incrementally
- emit a final chunk containing `finish_reason`
- terminate with `data: [DONE]`

### Streaming error behavior

If the upstream stream fails before any downstream bytes are emitted, the proxy returns a normal JSON error response with an appropriate HTTP status.

If the upstream stream fails after downstream streaming has begun, the proxy emits a terminal SSE error chunk if possible and closes the stream. The proxy does not fabricate a successful `[DONE]` after an unrecoverable upstream failure.

### Streaming assumptions in v1

- single choice only
- text output supported
- function call streaming supported for function tools
- unsupported event types are fatal if they are required for correct reconstruction
- irrelevant informational upstream events may be ignored only when the omission does not alter downstream semantics

## Error Model

All non-streaming failures return a JSON body with this shape:

```json
{
  "error": {
    "message": "human readable error",
    "type": "invalid_request_error",
    "code": "unsupported_feature",
    "param": "response_format",
    "request_id": "req_123"
  }
}
```

### Error properties

- `message`: human-readable description
- `type`: stable high-level class
- `code`: stable machine-readable code
- `param`: optional request field path
- `request_id`: proxy request identifier

### Error classes

- `invalid_request_error`
- `authentication_error`
- `permission_error`
- `rate_limit_error`
- `api_error`

### Status code policy

- `400` malformed JSON, structurally invalid request body
- `401` missing or invalid auth
- `403` forbidden by proxy auth policy
- `404` unknown route
- `405` method not allowed
- `408` upstream timeout if exposed as request timeout
- `409` state conflict when a request contains conflicting compatible parameters
- `413` body too large
- `415` unsupported content type
- `422` unsupported but understood request shape
- `429` upstream or local rate limit
- `500` internal proxy bug or unclassified local failure
- `502` invalid or untranslatable upstream response
- `503` upstream unavailable
- `504` upstream timeout

### Stable error codes

The implementation should prefer stable `code` values such as:

- `invalid_json`
- `unsupported_media_type`
- `missing_required_field`
- `unknown_field`
- `unsupported_feature`
- `conflicting_parameters`
- `invalid_parameter`
- `body_too_large`
- `unauthorized`
- `forbidden`
- `upstream_timeout`
- `upstream_unavailable`
- `upstream_error`
- `upstream_invalid_response`
- `upstream_translation_failed`
- `stream_translation_failed`
- `internal_error`

## Header Policy

### Forwarded inbound headers

- `Authorization` when pass-through auth mode is enabled
- `x-request-id` may be accepted from the caller if configured

### Generated proxy headers

- `x-request-id`

### Not forwarded by default

- arbitrary hop-by-hop headers
- inbound `Host`
- inbound `Content-Length`

## Configuration

Configuration is loaded at startup and validated strictly. Invalid configuration prevents startup.

### Required environment variables

- `OPENAI_BASE_URL`
- `OPENAI_API_KEY` unless pass-through upstream auth mode is enabled

### Optional environment variables

- `LISTEN_ADDR`
- `REQUEST_TIMEOUT_SECS`
- `CONNECT_TIMEOUT_SECS`
- `MAX_REQUEST_BODY_BYTES`
- `INBOUND_AUTH_MODE`
- `INBOUND_BEARER_TOKEN`
- `FORWARD_USER_FIELD`
- `TRUST_INBOUND_X_REQUEST_ID`
- `LOG_JSON`

### Configuration guarantees

- invalid URLs fail startup
- zero or nonsensical timeout values fail startup when disallowed
- missing bearer token in static auth mode fails startup
- contradictory auth settings fail startup

## Robustness Requirements

- no silent field drops unless explicitly documented and proven semantics-preserving
- no default fallbacks for invalid values
- no assuming upstream output shape without validation
- no truncating malformed streaming event sequences into fake success
- every request path receives a request ID
- every error path is logged with structured context and redaction

## Observability

### Logging

- structured logs via `tracing`
- request method, path, status, latency, request ID
- upstream status and latency
- no secrets in logs
- request bodies are not logged by default

### Health endpoints

- `GET /healthz` returns process health
- `GET /readyz` returns readiness based on local config validity and runtime initialization

## Security

- secrets are loaded from environment, never hardcoded
- auth headers are redacted from logs and errors
- body size is capped
- timeouts are enforced
- content type is validated strictly
- unsupported multimodal payloads are rejected instead of partially interpreted

## Testing Requirements

The implementation should include:

- unit tests for request validation
- unit tests for request translation
- unit tests for response translation
- unit tests for SSE event translation
- integration tests against a mocked upstream server
- error-path tests for malformed upstream JSON and malformed SSE

## Initial Project Layout

```text
src/
  main.rs
  app.rs
  config.rs
  error.rs
  observability.rs
  state.rs
  routes/
    mod.rs
    health.rs
    chat_completions.rs
  openai/
    mod.rs
    chat.rs
    responses.rs
  translate/
    mod.rs
    request.rs
    response.rs
    stream.rs
  upstream.rs
tests/
```

## Roadmap

Potential future work after the strict v1 path is solid:

- multimodal input support
- richer upstream response item support
- configurable compatibility profiles for non-OpenAI providers
- metrics export
- local rate limiting
- richer readiness checks

## Development Principle

If a request or response shape cannot be mapped precisely and safely, the proxy must fail explicitly.
