#!/usr/bin/env bash
#
# Nexus VM installer — Ubuntu 24.04 (Hetzner CX22 or larger).
#
# Installs: Postgres 16 + pgvector, Rust toolchain, Ollama + local models,
# builds the release binary, and registers a systemd service.
#
# Idempotent: safe to re-run. Run as root (or with sudo):
#   sudo bash install.sh
#
# Configure behaviour with env vars before running, e.g.:
#   DB_PASSWORD=secret EMBED_MODEL=nomic-embed-text CHAT_MODEL=qwen2.5:3b-instruct \
#   INSTALL_CHAT_MODEL=0 sudo -E bash install.sh
#
set -euo pipefail

# ── Tunables ────────────────────────────────────────────────────────────────
APP_USER="${APP_USER:-nexus}"
APP_DIR="${APP_DIR:-/opt/nexus}"
ENV_FILE="${APP_DIR}/.env"
DB_NAME="${DB_NAME:-nexus}"
DB_USER="${DB_USER:-nexus}"

# Resolve the DB password idempotently: explicit env wins; otherwise reuse the
# one already in an existing .env (so re-runs don't rotate it out of sync with
# what the service uses); otherwise generate a fresh one for a first install.
if [[ -z "${DB_PASSWORD:-}" && -f "${ENV_FILE}" ]]; then
  DB_PASSWORD="$(sed -nE 's#^DATABASE_URL=postgresql://[^:]+:([^@]+)@.*#\1#p' "${ENV_FILE}" | head -1)"
fi
DB_PASSWORD="${DB_PASSWORD:-$(openssl rand -hex 16)}"
EMBED_MODEL="${EMBED_MODEL:-mxbai-embed-large}"  # 1024-dim; encoder, fast on CPU
EMBED_DIM="${EMBED_DIM:-1024}"
CHAT_MODEL="${CHAT_MODEL:-qwen2.5:3b-instruct}"  # Tier 2 (summaries/excerpts); CPU-sized
INSTALL_CHAT_MODEL="${INSTALL_CHAT_MODEL:-1}"     # 1 = pull local chat model (Tier 2 is local by default)
PG_VERSION="${PG_VERSION:-16}"

log() { echo -e "\033[1;36m[nexus]\033[0m $*"; }

if [[ $EUID -ne 0 ]]; then
  echo "Please run as root (sudo bash install.sh)." >&2
  exit 1
fi

# ── 1. System packages ──────────────────────────────────────────────────────
log "Installing system packages…"
export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y --no-install-recommends \
  build-essential pkg-config libssl-dev ca-certificates curl git \
  "postgresql-${PG_VERSION}" "postgresql-${PG_VERSION}-pgvector"

systemctl enable --now postgresql

# ── 2. Database + role + extension ───────────────────────────────────────────
log "Configuring Postgres (db=${DB_NAME}, user=${DB_USER})…"
sudo -u postgres psql -v ON_ERROR_STOP=1 <<SQL
DO \$\$
BEGIN
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '${DB_USER}') THEN
    CREATE ROLE ${DB_USER} LOGIN PASSWORD '${DB_PASSWORD}';
  ELSE
    ALTER ROLE ${DB_USER} WITH PASSWORD '${DB_PASSWORD}';
  END IF;
END
\$\$;
SQL
# CREATE DATABASE can't run inside the DO block / a transaction.
if ! sudo -u postgres psql -tAc "SELECT 1 FROM pg_database WHERE datname='${DB_NAME}'" | grep -q 1; then
  sudo -u postgres createdb -O "${DB_USER}" "${DB_NAME}"
fi
sudo -u postgres psql -d "${DB_NAME}" -v ON_ERROR_STOP=1 -c "CREATE EXTENSION IF NOT EXISTS vector;"

# ── 3. App user + source ─────────────────────────────────────────────────────
if ! id "${APP_USER}" &>/dev/null; then
  log "Creating service user ${APP_USER}…"
  useradd --system --create-home --shell /usr/sbin/nologin "${APP_USER}"
fi

log "Syncing source into ${APP_DIR}…"
mkdir -p "${APP_DIR}"
# If running from a checkout, copy it in; otherwise assume it's already there.
SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ "${SRC_DIR}" != "${APP_DIR}" ]]; then
  cp -r "${SRC_DIR}/." "${APP_DIR}/"
fi
chown -R "${APP_USER}:${APP_USER}" "${APP_DIR}"

# ── 4. Rust toolchain (installed for the app user) ───────────────────────────
log "Installing Rust toolchain…"
sudo -u "${APP_USER}" bash -c '
  if ! command -v cargo >/dev/null 2>&1 && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
    curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  fi
'

# ── 5. Ollama + local models ─────────────────────────────────────────────────
log "Installing Ollama…"
if ! command -v ollama >/dev/null 2>&1; then
  curl -fsSL https://ollama.com/install.sh | sh
fi
# On a constrained box, keep one model resident and one request at a time so a
# 7B model can't get loaded twice and blow up RAM. Our ingestion queue is
# sequential anyway, so parallelism buys nothing here.
mkdir -p /etc/systemd/system/ollama.service.d
cat >/etc/systemd/system/ollama.service.d/override.conf <<EOF
[Service]
Environment=OLLAMA_NUM_PARALLEL=1
Environment=OLLAMA_MAX_LOADED_MODELS=1
EOF
systemctl daemon-reload
systemctl enable --now ollama
systemctl restart ollama

# GPU note: Ollama uses a GPU automatically if an NVIDIA driver is present.
if command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi >/dev/null 2>&1; then
  log "NVIDIA GPU detected — Ollama will run models on the GPU."
else
  log "No GPU detected — Ollama runs on CPU (expected for this box). Chat models"
  log "  are CPU-sized (${CHAT_MODEL}); embeddings run fast on CPU regardless."
fi
# Wait for the daemon to accept connections.
for _ in $(seq 1 30); do
  curl -sf http://localhost:11434/api/tags >/dev/null 2>&1 && break
  sleep 1
done

log "Pulling embedding model: ${EMBED_MODEL}…"
ollama pull "${EMBED_MODEL}"
if [[ "${INSTALL_CHAT_MODEL}" == "1" ]]; then
  log "Pulling local chat model: ${CHAT_MODEL} (used by tiers routed to ollama)…"
  ollama pull "${CHAT_MODEL}"
fi

# ── 5b. YouTube tooling (yt-dlp) ─────────────────────────────────────────────
# Standalone Linux build — no Python runtime dependency. Used to fetch video
# transcripts. If this box isn't x86_64, swap the asset for yt-dlp_linux_aarch64.
log "Installing yt-dlp (YouTube transcript fetcher)…"
curl -fsSL https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux \
  -o /usr/local/bin/yt-dlp
chmod a+rx /usr/local/bin/yt-dlp
/usr/local/bin/yt-dlp --version >/dev/null 2>&1 \
  && log "yt-dlp $(/usr/local/bin/yt-dlp --version) ready" \
  || log "WARN: yt-dlp install check failed — YouTube links will store a stub until fixed."

# ── 6. .env scaffold ─────────────────────────────────────────────────────────
if [[ ! -f "${ENV_FILE}" ]]; then
  log "Writing ${ENV_FILE} (fill in TELEGRAM_BOT_TOKEN + ANTHROPIC_API_KEY)…"
  cp "${APP_DIR}/.env.example" "${ENV_FILE}"
  sed -i "s#^DATABASE_URL=.*#DATABASE_URL=postgresql://${DB_USER}:${DB_PASSWORD}@localhost:5432/${DB_NAME}#" "${ENV_FILE}"
  sed -i "s#^EMBEDDING_MODEL=.*#EMBEDDING_MODEL=${EMBED_MODEL}#" "${ENV_FILE}"
  sed -i "s#^EMBEDDING_DIM=.*#EMBEDDING_DIM=${EMBED_DIM}#" "${ENV_FILE}"
  sed -i "s#^OLLAMA_CHAT_MODEL=.*#OLLAMA_CHAT_MODEL=${CHAT_MODEL}#" "${ENV_FILE}"
  chown "${APP_USER}:${APP_USER}" "${ENV_FILE}"
  chmod 600 "${ENV_FILE}"
else
  log ".env already exists — leaving it untouched."
fi

# ── 7. Build release binary ──────────────────────────────────────────────────
log "Building release binary (this takes a few minutes)…"
sudo -u "${APP_USER}" bash -c "cd '${APP_DIR}' && \$HOME/.cargo/bin/cargo build --release"

# ── 8. systemd service ───────────────────────────────────────────────────────
log "Installing systemd service…"
cat >/etc/systemd/system/nexus.service <<EOF
[Unit]
Description=Nexus Bot
After=network.target postgresql.service ollama.service
Wants=ollama.service

[Service]
Type=simple
User=${APP_USER}
WorkingDirectory=${APP_DIR}
EnvironmentFile=${APP_DIR}/.env
ExecStart=${APP_DIR}/target/release/nexus
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable nexus

cat <<DONE

────────────────────────────────────────────────────────────
 Nexus installed.

 1. Edit ${ENV_FILE} and set:
      TELEGRAM_BOT_TOKEN=...
      ANTHROPIC_API_KEY=...        (used for graph + /ask; Tier 2 runs locally)
    DB URL, embedding + chat model settings are already filled in.

 2. Start it:
      systemctl start nexus
      journalctl -u nexus -f

 DB password (saved in .env):  ${DB_PASSWORD}
 Embedding model:              ${EMBED_MODEL} (dim ${EMBED_DIM})
 Local chat model pulled:      $([[ "${INSTALL_CHAT_MODEL}" == "1" ]] && echo "${CHAT_MODEL}" || echo "no (set INSTALL_CHAT_MODEL=1 to enable)")
────────────────────────────────────────────────────────────
DONE
