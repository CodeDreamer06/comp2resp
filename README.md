# comp2resp

`comp2resp` is a Rust HTTP proxy that accepts an OpenAI-compatible `POST /v1/responses` request, translates it into an upstream OpenAI `POST /v1/chat/completions` request, and returns a Responses-compatible response.

The project is intentionally strict. Unsupported or ambiguous request shapes are rejected with explicit machine-readable errors instead of being silently ignored, partially translated, or coerced into a best-effort request.

## Status

This project is being built spec-first. The implementation follows this README contract.

## Goals

- Accept OpenAI-compatible Responses API requests.
- Translate them into OpenAI Chat Completions API requests.
- Return Responses-compatible responses.
- Support both non-streaming and streaming requests.
- Preserve correctness over broad compatibility.
- Emit explicit errors for invalid or unsupported behavior.

## Non-Goals

- Acting as a general OpenAI API reverse proxy.
- Supporting multiple upstream providers in the first version.
- Silently accepting loosely compatible or partially mappable request fields.
- Preserving undocumented behavior from third-party OpenAI-compatible providers.
- Supporting every historical responses field in the first release.

## Supported Surface

### Inbound endpoint

- `POST /v1/responses`

### Operational endpoints

- `GET /healthz`
- `GET /readyz`

### Upstream endpoint

- `POST {OPENAI_BASE_URL}/v1/chat/completions`

The first version targets Chat Completions semantics on the upstream side.

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
- `input`

### Supported request fields in v1

- `model`
- `input`
- `stream`
- `temperature`
- `top_p`
- `max_output_tokens`
- `tools`
- `tool_choice`

### Rejected request fields in v1

The proxy rejects these with `422 unsupported_feature` because they either do not map safely to the Chat Completions API contract used here or require additional compatibility semantics not implemented in v1:

- `metadata`
- `instructions`
- `previous_response_id`
- `truncation`
- any unknown top-level field

If a field is unsupported, the request fails. The proxy does not drop it and continue.

## Input Compatibility

### Supported input item types

- `message` with `role` and `content`
- `function_call`
- `function_call_output`

### Supported message roles

- `system`
- `user`
- `assistant`

### Supported message content forms

- string content
- array content containing only `input_text` parts

### Rejected input shapes in v1

- image, audio, or file parts
- unknown input item types
- messages missing required fields
- empty content

## Translation Rules

### Request translation: responses -> chat completions

The proxy converts the Responses request into a Chat Completions request using these rules.

### Model

- `model` is forwarded as-is.

### Stream

- `stream: true` becomes upstream `stream: true`.
- `stream: false` or omitted becomes upstream `stream: false`.

### Input construction

Inbound `input` items become Chat Completions `messages`.

Mapping rules:

- `message` item with role -> chat message with same role and text content
- `function_call` item -> assistant message with `tool_calls`
- `function_call_output` item -> tool message with `tool_call_id` and output content

### Token limit fields

- `max_output_tokens` maps to upstream `max_tokens`.

### Sampling fields

- `temperature` is forwarded.
- `top_p` is forwarded.

Validation rules:

- `temperature` must be finite.
- `top_p` must be finite and within supported bounds.

### Tools

- Responses `tools` of type `function` map to Chat Completions tools of type `function`.
- Tool schema is preserved.
- Unsupported tool types are rejected.

### Tool choice

- `none`, `auto`, and named function selection are supported when representable upstream.
- `required` is rejected in v1.
- Unsupported tool choice shapes are rejected.

## Response Translation

### Non-streaming responses

The proxy converts a completed Chat Completions response into a single Responses-compatible response object.

Returned top-level fields:

- `id`
- `created_at`
- `model`
- `output`
- `usage`
- `status`
- `incomplete_details` when applicable

### Output semantics

The proxy synthesizes `output` items from the upstream assistant message:

- Plain text content becomes a `message` output item with `output_text` content parts.
- `tool_calls` become `function_call` output items.

### Status mapping

Supported finish reasons from upstream:

- `stop` -> `status: "completed"`
- `length` -> `status: "incomplete"` with `reason: "max_output_tokens"`
- `tool_calls` -> `status: "completed"`
- `content_filter` -> `status: "incomplete"` with `reason: "content_filter"`

If the upstream response cannot be mapped to one of these safely, the proxy fails with `502 upstream_translation_failed` instead of guessing.

### Usage mapping

When usage is present upstream, the proxy maps it into Responses usage fields:

- `prompt_tokens` -> `input_tokens`
- `completion_tokens` -> `output_tokens`
- `total_tokens` -> `total_tokens`

If usage is absent or structurally incompatible, the proxy returns `usage: null` only if that behavior is valid for the response shape being emitted. Otherwise it fails explicitly.

## Streaming Contract

### Inbound

If the caller sends `stream: true`, the proxy opens an SSE request to the upstream Chat Completions API and emits Responses-style SSE events.

### Outbound content type

- `Content-Type: text/event-stream`
- `Cache-Control: no-cache`
- `Connection: keep-alive`

### Event translation

Chat Completions SSE chunks are translated into Responses semantic events.

Streaming behavior:

- emit `response.created` on first assistant role chunk
- emit `response.output_item.added` and `response.content_part.added` for the text message
- emit `response.output_text.delta` for each content delta
- emit `response.output_text.done`, `response.content_part.done`, `response.output_item.done`, and `response.completed` on finish
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
    "param": "metadata",
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
    responses.rs
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
