# Nexus

A group ambient intelligence layer for Telegram.

Add **@your_bot** to a group (or DM it). It silently watches every link shared,
ingests the content, builds a per-group knowledge graph, scores relevance against
each member's interest vector, and fires a **personal DM** when something one
person shared is highly relevant to another. Tag it or use `/ask` to query the
group's collective memory.

No digests, no noise. Invisible until it has something worth saying.

---

## Architecture

Single Rust binary on one VM. Everything runs in-process on one Tokio runtime:

```
Telegram update → handler ──┬─ URL?      → context window → ingestion queue
                            ├─ forward?  → ingestion queue
                            └─ /ask | @mention → query handler (RAG)

ingestion queue → fetch (Tier 1) → summarize (Tier 2 Haiku)
               → embed (Tier 3) → store (pgvector) → relevance scorer → DM

cron  6h  → graph builder (Tier 4 Sonnet): entities + edges
cron 24h  → purge message buffer, log group stats
cron 15m  → health check + retry failed fetches
```

| Tier | Default model | Provider | Used for |
|------|---------------|----------|----------|
| 1 | none | — | URL detect, fetch, Readability-style extraction, dedup |
| 2 | `qwen2.5:3b-instruct` (local) | `TIER2_PROVIDER` (anthropic \| ollama) | summarize, tag, classify, relevance excerpt |
| 3 | `mxbai-embed-large` (1024-dim) | local Ollama (or OpenAI/Voyage) | item + query + interest vectors |
| 4 | Claude Sonnet | `GRAPH_PROVIDER` | entity / edge extraction (graph build) |
| 5 | Claude Sonnet | `RAG_PROVIDER` | RAG answer synthesis |

Every chat tier is independently routable to Claude or a local Ollama model via
the `*_PROVIDER` env vars; embeddings run locally by default (no OpenAI account
needed). See **Local models** below.

### Module map

```
src/
  main.rs          wiring: pool, clients, queue, consumer, cron, dispatcher
  config.rs        typed env config
  models.rs        shared domain types (ContextWindow is the moat)
  state.rs         AppState injected everywhere
  bot/             dispatcher, message handler, commands, HTML formatter
  ingestion/       queue consumer, fetcher (extraction), pipeline (Tier 1→3)
  llm/             ModelTier, Anthropic client, embedding client
  db/              sqlx access: groups, items, profiles, entities, edges, notifications
  scorer/          vector math + the relevance interrupt
  graph/           Tier 4 batch entity/edge builder
  query/           embed → pgvector → Tier 5 → formatted reply
  cron/            scheduler + job bodies
migrations/        001..005 (schema, pgvector, profiles, buffer, notifications)
```

> **Two things that must never be refactored away:** the `context_window` on every
> item (the conversation around a shared link) and the relevance-interrupt DM.

---

## Local development (macOS)

### 1. Postgres 16 + pgvector

Easiest — Docker (no models, just the DB):

```bash
docker compose up -d      # Postgres 16 + pgvector on :5432, extension auto-created
# DATABASE_URL=postgresql://nexus:nexus@localhost:5432/nexus
```

Or natively:

```bash
brew install postgresql@16 pgvector && brew services start postgresql@16
createdb nexus && psql nexus -c 'CREATE EXTENSION IF NOT EXISTS vector;'
```

The dimensioned vector columns + ANN index are created at startup from
`EMBEDDING_DIM`, so the DB role just needs to own the `nexus` database.

### 2. Embeddings on your Mac

You said you don't want models on the Mac (storage). Two options:

- **Point at the VM's Ollama:** set `EMBEDDING_BASE_URL=http://<vm-ip>:11434/v1/embeddings`
  (and `OLLAMA_BASE_URL` likewise if routing chat tiers to it). Simplest for local testing.
- **Run without embeddings:** the bot still boots and stores items; only vector
  search / relevance scoring are skipped (logged as warnings). Fine for exercising
  the handler and DB paths.

### 3. Configure + run

```bash
cp .env.example .env
# required: TELEGRAM_BOT_TOKEN, ANTHROPIC_API_KEY, DATABASE_URL
cargo run            # migrations + vector schema applied automatically on boot
```

Migrations are embedded and applied at startup — no separate `sqlx migrate` step.

### Tests

```bash
cargo test                                  # unit tests (no DB needed)

# Integration test against a real DB:
docker compose up -d
TEST_DATABASE_URL=postgresql://nexus:nexus@localhost:5432/nexus \
  cargo test --test db_roundtrip -- --ignored --nocapture
```

---

## Telegram setup (important)

1. **Create the bot** — message [@BotFather](https://t.me/BotFather) → `/newbot`,
   copy the token into `TELEGRAM_BOT_TOKEN`.
2. **Disable privacy mode** — BotFather → `/setprivacy` → select your bot →
   **Disable**. Without this, Telegram only delivers messages that mention the
   bot or are commands, and Nexus can't watch ambient links.
3. **Add the bot to a group** (as a member; admin not required once privacy is off).
4. **Each user must DM the bot `/start` once.** Bots cannot initiate DMs, so a
   member who has never opened a chat with the bot will not receive relevance
   notifications until they do. Nexus logs (not crashes) on such failures.
5. Deep links (`t.me/c/...`) only resolve for **supergroups**, not basic groups
   or DMs.

---

## Bot commands

```
/ask [question]      query the group's knowledge graph
/stats               items ingested, top tags, date range, pings sent
/threshold [0.0-1.0] set your relevance sensitivity (lower = more pings; default 0.72)
/mute                pause your notifications for 24h
/unmute              resume notifications
/help                what the bot does
```

You can also just `@mention` the bot with a question.

---

## Local models

Embeddings and Tier-2 chat run **locally via Ollama**; graph + `/ask` use Claude.
Each chat tier is independently routable, so you tune the split to your hardware.

**Default layout (CPU box — e.g. `m4.metal.small`, AMD EPYC 4244P, 6c/12t, 64 GB):**

| Tier | Runs on | Model | Why |
|------|---------|-------|-----|
| 3 — embeddings | **local Ollama** | `mxbai-embed-large` (1024-dim) | it's an encoder — fast on CPU (~50–150 ms); 512-tok ctx is fine (we embed only distilled signal) |
| 2 — summarize/excerpt | **local Ollama** | `qwen2.5:3b-instruct` | ~10–18 s/summary on 6 cores; runs in the background queue, not user-facing |
| 4 — graph build (cron) | Claude | Sonnet | 20-item batch reasoning would crawl on CPU |
| 5 — `/ask` (interactive) | Claude | Sonnet | user is waiting — CPU generation would take 30–60 s |

**The CPU reality:** generation speed is capped by cores, not RAM. Encoders
(embeddings) fly; autoregressive chat does not. So local makes sense for
embeddings + the async Tier 2, while the reasoning-heavy / latency-sensitive
tiers stay on Claude.

**Tuning knobs (all in `.env`):**

- Better tags, slower ingestion → `OLLAMA_CHAT_MODEL=qwen2.5:7b-instruct` (the Zen4 chip handles it).
- Fastest ingestion → `TIER2_PROVIDER=anthropic` (Claude Haiku does summaries in ~1 s).
- **No GPU but want fast + cheap chat** → point the chat knobs at a hosted OSS provider (the client is OpenAI-compatible): Groq `llama-3.3-70b-versatile` (~$0.59/M, hundreds of tok/s) or DeepSeek `deepseek-chat` (~$0.28/M). Set `OLLAMA_BASE_URL`, `OLLAMA_CHAT_MODEL`, `OLLAMA_API_KEY` and route tiers to `ollama`. ~100× cheaper than Claude, `/ask` in seconds. See `.env.example` for exact values.
- Swap embedding model → change **both** `EMBEDDING_MODEL` and `EMBEDDING_DIM` (vector columns are created at that dim on first boot — changing it needs a fresh DB).
- **GPU box only:** fully local (`GRAPH_PROVIDER=ollama`, `RAG_PROVIDER=ollama`, `qwen2.5:32b-instruct`). On CPU, 32B answers `/ask` in minutes — don't.

---

## Deployment (any Ubuntu 24.04 VM / bare metal)

One script does everything — Postgres + pgvector, Rust, Ollama + models, the
release build, and a systemd service. First get the code onto the VM **into your
home dir** (a non-root user can't write to `/opt`, and rsync can't sudo
mid-transfer); `install.sh` then copies it to `/opt/nexus` itself.

```bash
# from your Mac (no git needed) — note the leading-slash-free target:
rsync -av --exclude target --exclude .env --exclude .git \
  /Users/you/Desktop/nexus/ ubuntu@VM_IP:nexus/

# on the VM:
ssh ubuntu@VM_IP
cd ~/nexus
sudo bash install.sh            # → installs everything, copies source to /opt/nexus
nano /opt/nexus/.env            # set TELEGRAM_BOT_TOKEN + ANTHROPIC_API_KEY
sudo systemctl start nexus
journalctl -u nexus -f
```

(Or, if you've pushed to GitHub: `git clone <repo> ~/nexus` instead of rsync.)

`install.sh` is idempotent and configurable via env vars — defaults are already
CPU-sized, but you can override:

```bash
# e.g. better tags via a 7B Tier-2 model:
CHAT_MODEL=qwen2.5:7b-instruct sudo -E bash install.sh
```

It writes a generated DB password into `/opt/nexus/.env` and fills in
`DATABASE_URL`, embedding + chat model settings automatically — you only add the
two tokens.

---

## Configuration reference

All knobs live in `.env` (see `.env.example`). Notable ones:

| Var | Default | Meaning |
|-----|---------|---------|
| `EMBEDDING_MODEL` / `EMBEDDING_DIM` | `nomic-embed-text` / `768` | embedding model + its vector dimension (must match) |
| `EMBEDDING_BASE_URL` | local Ollama | OpenAI-compatible `/v1/embeddings` endpoint |
| `EMBEDDING_API_KEY` | — | only for hosted embedding providers (OpenAI/Voyage) |
| `TIER2_PROVIDER` / `GRAPH_PROVIDER` / `RAG_PROVIDER` | `anthropic` | per-tier chat backend: `anthropic` or `ollama` |
| `OLLAMA_BASE_URL` / `OLLAMA_CHAT_MODEL` | localhost / `qwen2.5:3b-instruct` | local chat server + model |
| `CONTEXT_WINDOW_WAIT_SECS` | 60 | how long to wait for trailing context after a link |
| `DEFAULT_RELEVANCE_THRESHOLD` | 0.72 | cosine threshold for a notification |
| `MAX_VECTOR_WEIGHT` | 100.0 | caps interest-vector accumulation so old items fade |
| `URL_DEDUP_DAYS` | 7 | suppress re-ingesting the same URL in a group |
| `NOTIFICATION_SCORE_LOG` | true | log below-threshold scores for calibration |
| `GRAPH_CRON_SCHEDULE` | `0 0 */6 * * *` | 6-field cron (sec min hour dom mon dow) |

---

## Notes on the content extractor

Nexus uses a dependency-light Readability-style extractor (`scraper` for
metadata + main-content selection, `html2text` as fallback) rather than a port
of Mozilla Readability. It strips nav/header/footer/sidebar chrome, pulls
`og:`/`<title>`/author/published metadata, and truncates to ~4000 tokens before
the Tier 2 call. Paywalls / JS-only SPAs degrade gracefully: the item is stored
with whatever metadata is available and marked `pending_retry`, still useful for
graph edges and notifications.

### YouTube (v2)

`youtube.com` / `youtu.be` links are routed to a transcript fetcher instead of
the HTML extractor: `yt-dlp -J` returns metadata + caption-track URLs in one
call, the English VTT captions are fetched and parsed to clean text, then summarized
and embedded like any article. Items are tagged `content_type = video`. Videos
without captions fall back to the description. Requires `yt-dlp` (installed by
`install.sh` as a standalone Linux binary; configurable via `YTDLP_PATH`).

## Not in v1

Weekly digests, web dashboard, X/Twitter, podcast ingestion, personal graphs,
cross-group signals, billing. The schema carries `source` + `content_type` and
graph tables so these slot in without migration pain.
