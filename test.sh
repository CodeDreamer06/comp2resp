#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# comp2resp live test script
# Run this yourself — it is NOT run by CI or the assistant.
# It logs everything to test-run.log for post-run confirmation.
# ---------------------------------------------------------------------------

LOG_FILE="test-run.log"
MODEL="cwy/moonshotai/kimi-k2.6"
LISTEN_ADDR="127.0.0.1:3456"
SERVER_PID=""

# --- configurable upstream (defaults to the one you provided) ----------------
export OPENAI_BASE_URL="${OPENAI_BASE_URL:-https://api.freetheai.xyz}"
export OPENAI_API_KEY="${OPENAI_API_KEY:-sta_13b1cff82f71216f6d967b215205c7fa52b30c51120305df}"

# ---------------------------------------------------------------------------
cleanup() {
    if [ -n "$SERVER_PID" ]; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] Shutting down server (PID $SERVER_PID)..." | tee -a "$LOG_FILE"
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# --- init log --------------------------------------------------------------
echo "=============================================================================" > "$LOG_FILE"
echo "comp2resp live test log" >> "$LOG_FILE"
echo "Started : $(date)" >> "$LOG_FILE"
echo "Model   : $MODEL" >> "$LOG_FILE"
echo "Upstream: $OPENAI_BASE_URL" >> "$LOG_FILE"
echo "=============================================================================" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

# --- build -----------------------------------------------------------------
echo "[$(date '+%Y-%m-%d %H:%M:%S')] Building comp2resp (release)..." | tee -a "$LOG_FILE"
if ! cargo build --release >> "$LOG_FILE" 2>&1; then
    echo "BUILD FAILED — see $LOG_FILE" | tee -a "$LOG_FILE"
    exit 1
fi

# --- start server ----------------------------------------------------------
echo "[$(date '+%Y-%m-%d %H:%M:%S')] Starting server on $LISTEN_ADDR..." | tee -a "$LOG_FILE"
LISTEN_ADDR="$LISTEN_ADDR" \
LOG_JSON="false" \
./target/release/comp2resp >> "$LOG_FILE" 2>&1 &
SERVER_PID=$!

# Wait for healthz (up to 10 s)
for i in {1..10}; do
    if curl -sf "http://$LISTEN_ADDR/healthz" > /dev/null 2>&1; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] Server healthy." | tee -a "$LOG_FILE"
        break
    fi
    sleep 1
done

if ! curl -sf "http://$LISTEN_ADDR/healthz" > /dev/null 2>&1; then
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] ERROR: Server did not become healthy in time." | tee -a "$LOG_FILE"
    exit 1
fi

# --- helper: run with retries ----------------------------------------------
run_with_retries() {
    local name="$1"
    local payload="$2"
    local out_file="$3"
    local check_pattern="$4"

    echo "" | tee -a "$LOG_FILE"
    echo "--- $name ---" | tee -a "$LOG_FILE"

    for attempt in 1 2 3; do
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] Attempt $attempt..." | tee -a "$LOG_FILE"

        if curl -s -X POST "http://$LISTEN_ADDR/v1/responses" \
            -H "Content-Type: application/json" \
            -d "$payload" \
            -o "$out_file" 2>>"$LOG_FILE"; then

            if [ -s "$out_file" ]; then
                echo "[$(date '+%Y-%m-%d %H:%M:%S')] Raw response:" | tee -a "$LOG_FILE"
                cat "$out_file" | tee -a "$LOG_FILE"
                echo "" | tee -a "$LOG_FILE"

                if grep -q "$check_pattern" "$out_file"; then
                    echo "[$(date '+%Y-%m-%d %H:%M:%S')] SUCCESS: $name passed." | tee -a "$LOG_FILE"
                    return 0
                else
                    echo "[$(date '+%Y-%m-%d %H:%M:%S')] WARNING: Response received but pattern '$check_pattern' not found." | tee -a "$LOG_FILE"
                fi
            else
                echo "[$(date '+%Y-%m-%d %H:%M:%S')] WARNING: Empty response file." | tee -a "$LOG_FILE"
            fi
        else
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] WARNING: curl failed." | tee -a "$LOG_FILE"
        fi

        if [ "$attempt" -lt 3 ]; then
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] Retrying in 3 seconds..." | tee -a "$LOG_FILE"
            sleep 3
        fi
    done

    echo "[$(date '+%Y-%m-%d %H:%M:%S')] FAILED: $name exhausted all 3 attempts." | tee -a "$LOG_FILE"
    return 1
}

# --- Test 1: non-streaming -------------------------------------------------
run_with_retries \
    "Non-streaming responses" \
    '{"model":"'"$MODEL"'","input":[{"role":"user","content":[{"type":"input_text","text":"Say hello in exactly one word."}]}]}' \
    "/tmp/comp2resp_nostream.json" \
    '"status"'

# --- Test 2: streaming -----------------------------------------------------
run_with_retries \
    "Streaming responses" \
    '{"model":"'"$MODEL"'","input":[{"role":"user","content":[{"type":"input_text","text":"Count to 3"}]}],"stream":true}' \
    "/tmp/comp2resp_stream.txt" \
    "data:"

# --- summary ---------------------------------------------------------------
echo "" | tee -a "$LOG_FILE"
echo "=============================================================================" | tee -a "$LOG_FILE"
echo "Test finished at $(date)" | tee -a "$LOG_FILE"
echo "Full log: $LOG_FILE" | tee -a "$LOG_FILE"
echo "=============================================================================" | tee -a "$LOG_FILE"
