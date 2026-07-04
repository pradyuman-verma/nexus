<h1 align="center">Nexus</h1>

<p align="center">
  <b>A context capture protocol.</b><br>
  Forward anything — links, notes, voice notes, photos — from any connected chat channel.
  Nexus builds a private knowledge brain over it and answers questions like a researcher,
  so nothing you meant to keep gets lost in the noise of social media.
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#channels">Channels</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="#the-retrieval-pipeline">Retrieval</a> ·
  <a href="#deployment">Deploy</a> ·
  <a href="#configuration">Config</a>
</p>

---

Social media is where you find things — and where you lose them. Nexus is the
proper home for everything you meant to keep: forward it to the Nexus bot on
whatever app you're already in, with zero typing or organizing. It ingests the
**full content** (articles _and_ YouTube transcripts; voice notes transcribed;
photos described), splits it into searchable passages, builds a **knowledge
graph**, learns **what you care about**, and surfaces intelligence two ways:

- **On demand** — ask a question (`/ask`, `@mention`, or just message the bot),
  and it runs a multi-stage retrieval pipeline over everything you've captured,
  then answers with cited sources.
- **Proactively** — in group spaces, when one person shares something highly
  relevant to another's interests, that person gets a **personal DM** with the
  exact relevant excerpt.

It's a **closed-corpus brain**: it only knows what you've captured, never the
open web. The LLM supplies reasoning; the knowledge is strictly your own
attention — a group's collective memory, or a personal capture inbox.

---

## Channels

One brain, many doors. Every channel feeds the same identity-mapped intake,
ingestion pipeline, and retrieval engine:

| Channel       | Status     | Transport                           | Captures                               |
| ------------- | ---------- | ----------------------------------- | -------------------------------------- |
| **Telegram**  | ✅ live    | long-polling bot                    | links, forwards, group context         |
| **WhatsApp**  | ✅ live    | Cloud API webhook (HTTPS via Caddy) | links, text notes, voice notes, photos |
| **Instagram** | 🔜 planned | Messaging API webhook               | reels, posts, DM forwards              |
| **X/Twitter** | 🔜 planned | DM webhook                          | tweets, threads                        |

Each WhatsApp sender gets a personal space: their captures build their own
brain, and questions in the same chat search only it. Internally every channel
maps its native ids (chat ids, phone numbers, handles) onto one id space via
`(channel, external_id)` — linking the same human across channels is on the
roadmap.

---

## Features

- 📥 **Zero-friction capture** — forward links, notes, voice notes, or photos to the bot on Telegram or WhatsApp; no commands, no folders, no typing.
- 🔗 **Silent link ingestion** — every URL shared is fetched, extracted, and indexed. No commands needed.
- 📺 **Articles + YouTube** — Readability-style extraction for pages; `yt-dlp` transcripts for videos.
- 🎙️ **Voice notes & photos** — WhatsApp voice notes are transcribed (Whisper via any OpenAI-compatible STT), photos described by Claude vision; both become first-class, searchable knowledge.
- 🧠 **Chunk-level RAG** — content is split into overlapping passages and embedded, so it reads the _whole_ document, not a summary.
- 🔎 **Hybrid retrieval** — semantic (pgvector) **+** keyword (Postgres full-text), fused with Reciprocal Rank Fusion.
- 🕸️ **Knowledge graph** — entities (people, companies, topics) and edges extracted across items; used to expand retrieval along connections.
- 🧭 **Multi-query / HyDE + self-correction** — expands your question into probes, and retries retrieval when the first pass falls short.
- 🎯 **The relevance interrupt** — per-user interest vectors trigger a personal DM when something genuinely relevant to _you_ is shared.
- 💬 **The context moat** — every item stores the 3–5 messages around the share, capturing _why_ it was shared, not just _what_.
- 🪙 **Cheap & swappable models** — local Ollama embeddings + any OpenAI-compatible chat endpoint (DeepSeek, Groq, local, or Claude), routable per tier.
- 🦀 **One Rust binary** — bot, ingestion worker, scorer, query engine, and cron all in-process. Postgres + pgvector alongside.

---

## How it works

```
Telegram update            WhatsApp webhook (signed POST)
      │                          │
      ▼                          ▼
 message handler          channel handler ── voice → STT · photo → vision
      │                          │
      └────────────┬─────────────┘
                   ▼
 intake (channel-agnostic) ─┬─ URL?      → context window → ingestion queue
                            ├─ note/voice/photo → pre-extracted → queue
                            └─ question  → query engine → reply on same channel

 ingestion worker (continuous):
   fetch (article | youtube | pre-extracted) → extract → summarize+tag (LLM)
     → chunk into passages → embed each (local) → store
     → relevance scorer → DM anyone it's relevant to

 query engine (/ask):
   expand (HyDE+multi-query) → hybrid search (vector+keyword, RRF)
     → graph expansion → cited synthesis → self-correct once if needed

 cron:  6h graph build · 24h cleanup+stats · 15m health+retry
```

### Model tiers

Each task uses the cheapest model that can do it. Chat tiers (2/4/5) are routed
independently to any OpenAI-compatible endpoint or Claude; embeddings (3) run
locally by default.

| Tier | Task                                                         | Default                                   |
| ---- | ------------------------------------------------------------ | ----------------------------------------- |
| 1    | URL detect, fetch, extraction, chunking, dedup               | none                                      |
| 2    | summarize, tag, classify, relevance excerpt, query expansion | chat model (`TIER2_PROVIDER`)             |
| 3    | embed passages, queries, interest vectors                    | local Ollama `mxbai-embed-large` (1024-d) |
| 4    | entity + edge extraction (graph)                             | chat model (`GRAPH_PROVIDER`)             |
| 5    | RAG answer synthesis                                         | chat model (`RAG_PROVIDER`)               |

---

## The retrieval pipeline

`/ask` is the centrepiece. It's a multi-stage retriever designed to "retrieve
broadly, reason narrowly":

1. **Query expansion (HyDE + multi-query)** — the LLM rewrites your question into
   2–3 alternative probes plus a hypothetical answer paragraph. All are embedded.
2. **Hybrid search** — for each embedding, a vector search over passages; plus a
   keyword (full-text) search that catches exact terms, names, and acronyms
   embeddings blur. All ranked lists are merged with **Reciprocal Rank Fusion**.
3. **Graph expansion** — entities named in the question seed a one-hop walk of the
   knowledge graph; items that _mention_ those entities are pulled in even if
   vector similarity missed them.
4. **Cited synthesis** — the top passages (grouped under their source items) go to
   the LLM, which answers in plain prose, grounds every claim in a numbered
   source, and lists only the sources it actually used.
5. **Self-correction** — if the model reports it can't answer, it returns
   `follow_up` queries; Nexus retrieves again targeting the gap and synthesizes
   once more before saying "nothing relevant yet."

Precision comes from three independent places — the keyword match, the graph
filter, and the model's own answerability gate — so no single threshold can
silently zero out results.

---

## Quick start (local dev, macOS/Linux)

**1. Postgres 16 + pgvector** — easiest via Docker:

```bash
docker compose up -d        # Postgres + pgvector on :5432, extension auto-created
```

**2. Models** — install [Ollama](https://ollama.com) and pull an embedding model
(used locally). For chat, either pull a local model too, or point the `OLLAMA_*`
knobs at a hosted OSS provider (DeepSeek / Groq) — see [Swapping the chat model](#swapping-the-chat-model).

```bash
ollama pull mxbai-embed-large       # embeddings (Tier 3)
ollama pull qwen2.5:3b-instruct     # local chat (Tiers 2/4/5) — or use DeepSeek/Groq
```

**3. Configure and run:**

```bash
cp .env.example .env
# set TELEGRAM_BOT_TOKEN, and an OLLAMA_API_KEY if using a hosted chat provider
cargo run                   # migrations + vector schema applied automatically
```

**Tests:**

```bash
cargo test                  # unit tests, no DB needed

# integration test against a real DB:
docker compose up -d
TEST_DATABASE_URL=postgresql://nexus:nexus@localhost:5432/nexus \
  cargo test --test db_roundtrip -- --ignored --nocapture
```

---

## Telegram setup

1. **Create the bot** — [@BotFather](https://t.me/BotFather) → `/newbot` → copy the token into `TELEGRAM_BOT_TOKEN`.
2. **Disable privacy mode** — BotFather → `/setprivacy` → **Disable**. Without this, Telegram only delivers commands/mentions and Nexus can't watch ambient links.
3. **Add the bot to a group** (member is enough once privacy is off).
4. **Each user DMs the bot `/start` once** — bots can't initiate DMs, so relevance notifications only reach members who've opened a chat with it.
5. Deep links (`t.me/c/…`) resolve only for **supergroups**.

---

## WhatsApp setup

WhatsApp runs over Meta's **Business Cloud API** (webhook-based — Meta POSTs
inbound messages to you over HTTPS). Prerequisites have review lead time, so
start them early:

1. **Meta developer app** — create one, add the **WhatsApp** product, note the
   **phone number id** and generate a **permanent system-user token** (the
   API-Setup test token expires in 24h).
2. **App secret** — App Settings → Basic → copy into `WA_APP_SECRET`. Every
   webhook POST is signature-verified against it; unsigned traffic gets 401.
3. **Expose the webhook** — deploy behind Caddy (or any TLS proxy; Meta requires
   HTTPS), then in WhatsApp → Configuration set:
   - Callback URL: `https://<your-domain>/webhook/whatsapp`
   - Verify token: the same string as `WA_VERIFY_TOKEN`
   - Subscribe to the **messages** field.
4. **Fill the `WA_*` vars** in `.env` — the webhook server only starts when all
   four are present.

No Meta app yet? Drive the pipeline locally with the signed-payload simulator:

```bash
./scripts/simulate_whatsapp.sh text "check this https://youtu.be/dQw4w9WgXcQ"
./scripts/simulate_whatsapp.sh text "where was that pasta place?"
```

What each message type does:

| You send                    | Nexus does                                                            |
| --------------------------- | --------------------------------------------------------------------- |
| a link (or forwarded post)  | full ingestion — fetch, summarize, chunk, embed, graph                |
| a text note                 | captured as a `note` item, same pipeline minus the fetch              |
| a voice note                | transcribed, then captured as a `voice` item                          |
| a photo (± caption)         | described by vision LLM, captured as an `image` item                  |
| a question                  | runs the full `/ask` retrieval pipeline over **your** captures        |

---

## Commands

```
/ask [question]      query the group's knowledge — full retrieval pipeline
/stats               items ingested, top tags, date range, pings sent
/threshold [0-1]     your relevance sensitivity (lower = more pings; default 0.72)
/mute   /unmute      pause / resume your relevance notifications
/ping                health check (DB + active models)
/buildgraph          build the knowledge graph now (instead of waiting for cron)
/reindex             backfill passage chunks for items ingested before chunking
/help                what the bot does
```

You can also just `@mention` the bot with a question.

---

## Deployment (any Ubuntu 24.04 VM / bare metal)

One script installs everything — Postgres + pgvector, Rust, Ollama + models,
`yt-dlp`, the release build, and a systemd service. It's idempotent.

```bash
# get the code onto the VM, into your home dir (a non-root user can't write /opt):
rsync -av --exclude target --exclude .env --exclude .git ./ user@VM_IP:nexus/

ssh user@VM_IP
cd ~/nexus
sudo bash install.sh        # → copies to /opt/nexus, builds, registers service
nano /opt/nexus/.env        # set TELEGRAM_BOT_TOKEN (+ chat provider key)
sudo systemctl start nexus
journalctl -u nexus -f
```

`install.sh` is configurable via env (`EMBED_MODEL`, `CHAT_MODEL`,
`INSTALL_CHAT_MODEL`, `DB_PASSWORD`, …) and fills `DATABASE_URL` + model settings
into `.env` for you — you only add the tokens.

**Hardware note:** embeddings (an encoder) are fast on CPU; LLM _generation_ is
not. On a GPU-less box, run embeddings locally and point the chat tiers at a cheap
hosted OSS endpoint (DeepSeek / Groq) for fast, near-free `/ask`. 64 GB RAM
doesn't make CPU generation fast — a 70B model fits but crawls.

---

## Configuration

Everything is set in `.env` (see [`.env.example`](.env.example)). Highlights:

| Var                                                        | Default                      | Meaning                                                                |
| ---------------------------------------------------------- | ---------------------------- | ---------------------------------------------------------------------- |
| `TELEGRAM_BOT_TOKEN`                                       | —                            | required                                                               |
| `DATABASE_URL`                                             | —                            | Postgres connection string                                             |
| `WA_ACCESS_TOKEN` / `WA_PHONE_NUMBER_ID` / `WA_APP_SECRET` / `WA_VERIFY_TOKEN` | unset        | WhatsApp channel — all four or none; enables the webhook server        |
| `PORT`                                                     | `8080`                       | webhook HTTP port (put Caddy in front for TLS)                         |
| `WA_ACK_ON_CAPTURE`                                        | `true`                       | reply "✓ saved" after each WhatsApp capture                            |
| `STT_BASE_URL` / `STT_MODEL` / `STT_API_KEY`               | Groq whisper                 | OpenAI-compatible transcription endpoint for voice notes               |
| `EMBEDDING_MODEL` / `EMBEDDING_DIM`                        | `mxbai-embed-large` / `1024` | embedding model + its dimension (must match; set at first boot)        |
| `EMBEDDING_BASE_URL`                                       | local Ollama                 | OpenAI-compatible `/v1/embeddings` endpoint                            |
| `TIER2_PROVIDER` / `GRAPH_PROVIDER` / `RAG_PROVIDER`       | `ollama`                     | per-tier chat backend: `ollama` (any OpenAI-compatible) or `anthropic` |
| `OLLAMA_BASE_URL` / `OLLAMA_CHAT_MODEL` / `OLLAMA_API_KEY` | local                        | chat endpoint — point at Ollama, DeepSeek, Groq, etc.                  |
| `YTDLP_PATH`                                               | `yt-dlp`                     | binary for YouTube transcripts                                         |
| `CONTEXT_WINDOW_WAIT_SECS`                                 | `60`                         | how long to wait for trailing context after a link                     |
| `DEFAULT_RELEVANCE_THRESHOLD`                              | `0.72`                       | cosine threshold for a relevance DM                                    |
| `MAX_VECTOR_WEIGHT`                                        | `100.0`                      | caps interest-vector accumulation so old items fade                    |
| `GRAPH_CRON_SCHEDULE`                                      | `0 0 */6 * * *`              | 6-field cron (sec min hour dom mon dow)                                |

> **Note:** this is a committed template — never put real secrets in `.env.example`.
> It's read by systemd's `EnvironmentFile`, which does **not** strip trailing
> `# comments`, so keep comments on their own lines.

### Swapping the chat model

`/ask`, summaries, and graph extraction can run on any OpenAI-compatible endpoint.
To use DeepSeek for all chat tiers, for example:

```env
TIER2_PROVIDER=ollama
GRAPH_PROVIDER=ollama
RAG_PROVIDER=ollama
OLLAMA_BASE_URL=https://api.deepseek.com/v1
OLLAMA_CHAT_MODEL=deepseek-chat
OLLAMA_API_KEY=sk-...
```

Changing `EMBEDDING_DIM` requires a fresh database (vector columns are created at
that dimension on first boot).

---

## Data model

| Table               | Purpose                                                                                                         |
| ------------------- | --------------------------------------------------------------------------------------------------------------- |
| `groups`, `users`   | spaces and people, any channel — `(channel, external_id)` maps native ids onto internal BIGINTs                 |
| `channel_events`    | webhook idempotency — Meta redeliveries are dropped on conflict                                                 |
| `user_profiles`     | per-user, per-group **interest vector** + relevance threshold + mute                                            |
| `items`             | one row per shared link: url, title, summary, tags, `content_type`, `context_window` (the moat), item embedding |
| `chunks`            | passage-level text + embedding (the `/ask` retrieval unit) + a full-text index                                  |
| `entities`, `edges` | the knowledge graph                                                                                             |
| `messages_buffer`   | 48h rolling buffer to reconstruct context windows                                                               |
| `notifications_log` | dedup + calibration log for relevance DMs                                                                       |

Migrations are embedded in the binary and applied at startup; the dimensioned
vector columns + ANN indexes are created at boot from `EMBEDDING_DIM`.

---

## Project structure

```
src/
  main.rs / lib.rs      wiring + library surface (so tests can drive internals)
  config.rs, state.rs   typed env config; shared AppState
  models.rs             domain types (ContextWindow, RetrievedItem, …)
  bot/                  teloxide dispatcher, message handler, commands, HTML formatter
  whatsapp/             Cloud API client, webhook types, signature verify, channel handler
  http.rs               axum server for webhook channels (only runs when configured)
  intake.rs             channel-agnostic capture: URL extraction, context windows, note scheduling
  ingestion/            queue consumer, fetcher, youtube transcripts, chunker, pipeline
  llm/                  ModelTier, chat router (per-tier provider), Ollama/Anthropic/embeddings/STT
  db/                   sqlx access: items, chunks, entities, edges, graph, profiles, channels, …
  scorer/               vector math + the relevance interrupt
  graph/                Tier 4 batch entity/edge builder
  query/                the retrieval pipeline (expand → hybrid → graph → synth → self-correct)
  cron/                 scheduler + jobs
migrations/             001–009 (schema, pgvector, profiles, buffer, notifications, content_type, chunks, FTS, channels)
install.sh              one-shot VM installer
docker-compose.yml      local Postgres + pgvector
```

---

## Roadmap

**Done:** silent ingestion · article + YouTube · chunk-level RAG · hybrid search ·
knowledge graph + graph-aware retrieval · multi-query/HyDE · self-correcting
synthesis · the relevance interrupt · per-tier model routing · **WhatsApp channel**
(links, notes, voice, photos) · channel-agnostic identity layer.

**Next:** Instagram + X/Twitter channels · cross-channel identity linking
("registered usernames": one human, many handles) · PDF support · a read-only
web dashboard (force-directed graph + timeline + search) · weekly resurfacing
digest · cross-group convergence signals.

The schema carries `channel`, `source`, and `content_type`, so new input
channels slot in without migration pain.

---

## Two things that must never be refactored away

1. **The `context_window` on every item** — the conversation around a shared link.
   "lol this is exactly what we're building" next to a fundraise link is metadata
   no bookmarking tool captures. It tells the graph _why_ something was shared.
2. **The relevance interrupt** — the personal DM with the exact relevant excerpt.
   Not a digest, not a summary — the moment the product feels like magic.

---

## License

MIT © Nexus contributors. See [LICENSE](LICENSE).
