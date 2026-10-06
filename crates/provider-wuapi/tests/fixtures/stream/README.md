# Event stream fixtures

What the wuapi event stream (`GET /v1/events/stream`, `text/event-stream`) sends, as this client expects it.

**Provenance.** Written by hand from the backend change `event-stream-sse` (specs `stream-contract`, `stream-auth-limits`, `stream-gateway`, and its `design.md`), not captured from a running gateway: the gateway was not deployed when these were made. The envelopes are built from the other fixtures in `tests/fixtures/` (message, account, story, contact, group, chat), so they decode with the SDK's own types.

**Diffed against the backend's contract fixtures on 2026-10-03** (`packages/stream-gateway/contract/` of the backend repository, which are the contract). Aligned: every stream but a heartbeat-only one begins with `retry: 3000`; the drain's `retry` is 7000 (the gateway draws 1000 to 20000 and the client assumes no value); `reset_unknown.sse` and `reset_not_logged.sse` were added (the client ends the stream on any `reset`, whatever its `reason`); the 503 body is `service_unavailable`. The files in `errors/` are byte for byte the backend's, except `not_api.html`, which is only ours.

**Different on purpose.** The envelopes in `data:` are built from this repository's SDK fixtures and are richer than the backend's simplified ones (`direction`, `from`, `text` only); `ticks`, `mixed`, `older_backend` and `undecodable` have no backend counterpart. Cursors and `evt_` ids, message ids and timestamps are ours (cursors are opaque to the client). The backend's cursor is a snapshot that may lag the event, so a duplicate is possible and a gap is not: the client dedupes on the `evt_` id and never reads a cursor. Filters (`types`, `accounts`) are not sent by this client; the gateway answers 400 for presence types, `webhook.test`, unknown types or more than 50 values. What is still unproven is the owner's live run against the deployed gateway.

Cursors (`id:`) follow the backend design (`c1.<base36 ms>`); the client treats them as opaque.

| File | What it holds | Backend requirement |
|---|---|---|
| `live.sse` | `retry: 3000`, then one `message.received` frame with `id`, `event`, `data` | stream-contract, "SSE framing": each event is a frame with `id`, `event`, `data`; the stream sends `retry` |
| `heartbeat.sse` | `: ping`, and `: ping` carrying an `id:` | stream-contract, "SSE framing" (a `: ping` every 15 s); design, spike 2: heartbeats carry `id:` when the watermark moved |
| `duplicate.sse` | The same `evt_` twice, under two cursors | stream-contract, "At-least-once delivery": the `evt_` id is stable across replays |
| `resume_before.sse`, `resume_after.sse` | Events 1 and 2, then close. After a reconnect: a replay of 2, then 3, then live 4 | stream-contract, "Resume and reset": a cursor within 30 minutes replays what was missed, then goes live; stream-gateway, "Graceful drain" |
| `reset.sse`, `reset_unknown.sse`, `reset_not_logged.sse` | `event: reset` with reason `cursor_expired`, `cursor_unknown` or `not_logged`, then a live frame | stream-contract, "Resume and reset": an older or unknown cursor yields an explicit `reset`; design, "Request handling": the stream continues live after it |
| `drain.sse` | One event, then `retry: 7000`, then close | stream-gateway, "Graceful drain": clients are asked to reconnect, resuming loses nothing |
| `ticks.sse` | `message.sent`, `.delivered`, `.read` of one message | stream-contract, "Envelope identical to webhook" (the webhook types and bodies) |
| `mixed.sse` | One frame of every type the client maps, a `call.received`, and a type no SDK knows with extra fields | stream-contract, "Envelope identical to webhook"; the client's own mapping table (stream-event-mapping) |
| `older_backend.sse` | A message without `forwardedManyTimes` and `media.gifPlayback`; an account without `imageQuality`; a `group.updated` without `linked` and `unlinked`; a `group.joined` without `communityId` and `default` | Not a backend requirement: a backend older than the SDK (see `src/compat.rs`) |
| `community.sse` | A subgroup linked, reported on the community; a subgroup unlinked, reported on the subgroup (`communityId` names the community both times); a `group.joined` inside a community; a `group.joined` of a community itself | Written by hand from the API spec of SDK 0.14.0 (`GroupChange.linked`, `unlinked`, `communityId`; `Group.communityId`, `default`), not captured from a running gateway |
| `undecodable.sse` | A `message.received` whose `from` is a number, with valid `accountId` and `chatId` | Not a backend requirement: the client's rule for a payload it cannot read |
| `errors/400.json` | Invalid filter, presence type in `types` | stream-contract, "Pre-stream errors": 400 in the `{code, message, details?}` shape |
| `errors/401.json` | Missing, malformed or revoked key | stream-auth-limits, "Header-only authentication"; backend `errors.ts` `AUTH_REJECTIONS.invalid` |
| `errors/403.json` | Suspended organization | stream-auth-limits, "Revocation closes open streams" (new connects get 403); `organization_suspended` |
| `errors/404.json` | `project_not_found` | stream-contract, "Pre-stream errors" |
| `errors/429_limit.json` | Concurrent stream cap reached | stream-auth-limits, "Free concurrency cap"; code `stream_connection_limit`, sent with `Retry-After` |
| `errors/429_rate.json` | Connect-rate limit | stream-auth-limits, "Separate connect-rate limit"; code `rate_limited`, sent with `Retry-After` |
| `errors/503.json` | Upstream unavailable | stream-contract, "Pre-stream errors": 503 with `Retry-After`. Code `service_unavailable`, as the backend has it |
| `errors/not_api.html` | A page that is not the API (for example a host in front of it answering 401) | Not a backend file: the client must not sign the user out on a 401 that is not API-shaped |
| `../chat.json` | One chat, as the single-chat read answers | The REST `GET /v1/accounts/{a}/chats/{c}` answer, taken from `chat_list.json` |
