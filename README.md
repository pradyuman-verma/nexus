<h1 align="center">Nexus</h1>

<p align="center">
  <b>A group ambient-intelligence layer for Telegram.</b><br>
  It silently reads every link your group shares, builds a private knowledge brain over it,
  and answers questions like a researcher — or DMs you the moment something relevant lands.
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="#the-retrieval-pipeline">Retrieval</a> ·
  <a href="#deployment">Deploy</a> ·
  <a href="#configuration">Config</a>
</p>

---

Add the bot to a Telegram group (or DM it). From then on it watches links in the
background, ingests their **full content** (articles _and_ YouTube transcripts),
splits it into searchable passages, builds a per-group **knowledge graph**, learns
**what each member cares about**, and surfaces intelligence two ways:

- **On demand** — `@mention` it or use `/ask`, and it runs a multi-stage retrieval
  pipeline over everything the group has shared, then answers with cited sources.
- **Proactively** — when one person shares something highly relevant to another's
  interests, that person gets a **personal DM** with the exact relevant excerpt.

It's a **closed-corpus brain**: it only knows what your group has shared, never
the open web. The LLM supplies reasoning; the knowledge is strictly your group's
collective attention.

---

## Features

- 🔗 **Silent link ingestion** — every URL shared is fetched, extracted, and indexed. No commands needed.
- 📺 **Articles + YouTube** — Readability-style extraction for pages; `yt-dlp` transcripts for videos.
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
Telegram update
      │
      ▼
 message handler ─┬─ URL?         → capture context window → ingestion queue
                  ├─ forwarded?   → capture origin          → ingestion queue
                  └─ /ask | @mention → query engine

 ingestion worker (continuous):
   fetch (article | youtube) → extract → summarize+tag (LLM)
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
| `groups`, `users`   | Telegram chats and members                                                                                      |
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
  ingestion/            queue consumer, fetcher, youtube transcripts, chunker, pipeline
  llm/                  ModelTier, chat router (per-tier provider), Ollama/Anthropic/embeddings
  db/                   sqlx access: items, chunks, entities, edges, graph, profiles, …
  scorer/               vector math + the relevance interrupt
  graph/                Tier 4 batch entity/edge builder
  query/                the retrieval pipeline (expand → hybrid → graph → synth → self-correct)
  cron/                 scheduler + jobs
migrations/             001–008 (schema, pgvector, profiles, buffer, notifications, content_type, chunks, FTS)
install.sh              one-shot VM installer
docker-compose.yml      local Postgres + pgvector
```

---

## Roadmap

**Done:** silent ingestion · article + YouTube · chunk-level RAG · hybrid search ·
knowledge graph + graph-aware retrieval · multi-query/HyDE · self-correcting
synthesis · the relevance interrupt · per-tier model routing.

**Next:** X/Twitter ingestion · PDF support · a read-only web dashboard
(force-directed graph + timeline + search) · podcast (Whisper) ingestion ·
cross-group convergence signals.

The schema already carries `source` and `content_type`, so new input channels
slot in without migration pain.

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
