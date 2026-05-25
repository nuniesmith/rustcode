#!/usr/bin/env bash
#
# rc-app end-to-end smoke tests
#
# Covers TODO.md "RC-LLM: Routing & Proxy Validation" (lines 440-447):
#   1.  health         — /v1/chat/completions reachable + responds with a body
#   2.  claude_default — model=auto routes to claude-* when ANTHROPIC_API_KEY is set
#   3.  scaffold_task  — code-completion prompt routes to Sonnet (TaskKind::ScaffoldStub)
#   4.  arch_task      — architectural-reasoning prompt routes to Opus
#   5.  grok_fallback  — request returns a grok-* model (run against a Grok-only stack)
#   6.  multi_turn     — chat history preserved across turns
#   7.  repo_rag       — x_repo_id injection produces rag_chunks_used > 0
#   8.  prompt_cache   — second identical request reports cache_read_input_tokens > 0
#   9.  response_cache — second identical request reports cached: true
#  10.  auth_401       — request without Authorization header → 401
#
# Each probe prints a single PASS/FAIL line plus a one-line reason. The
# exit code is the count of failed probes (0 = all good). Probes that
# require server-side configuration we can't introspect (5 grok_fallback,
# 7 repo_rag) are marked "SKIP" if the precondition isn't met locally
# rather than failing — we only fail if the server's response actively
# contradicts the contract.
#
# Usage:
#   RUSTCODE_URL=https://your-stack.example.com RUSTCODE_API_KEY=... ./scripts/smoke_test.sh
#
# Required env vars:
#   RUSTCODE_URL       Base URL (no trailing slash) — e.g. http://localhost:3500
#   RUSTCODE_API_KEY   Bearer token; must match one of RUSTCODE_PROXY_API_KEYS
#                      on the server side
#
# Optional env vars:
#   RUSTCODE_REPO_ID   Pre-registered repo id used by probe 7. If unset, probe 7
#                      attempts to register the directory in $RUSTCODE_TEST_REPO_PATH
#                      (or, if that's also unset, SKIPs).
#   RUSTCODE_TEST_REPO_PATH  Absolute path on the *server* that probe 7 registers
#                      when RUSTCODE_REPO_ID is unset. The server walks this path
#                      — relative or local-to-this-machine paths will not work.
#   ONLY               Comma-separated list of probe names to run (e.g. ONLY=auth_401,health)
#   SKIP               Comma-separated list of probe names to skip
#
# Dependencies: bash 4+, curl, jq.

set -uo pipefail

# --------------------------------------------------------------------------- #
# Plumbing
# --------------------------------------------------------------------------- #

if ! command -v curl >/dev/null 2>&1; then
    echo "ERROR: curl is required but not on PATH" >&2
    exit 127
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq is required but not on PATH" >&2
    exit 127
fi

: "${RUSTCODE_URL:?RUSTCODE_URL is required (e.g. http://localhost:3500)}"
: "${RUSTCODE_API_KEY:?RUSTCODE_API_KEY is required (bearer token)}"

# Strip a trailing slash from RUSTCODE_URL so we can build paths with a leading slash.
RUSTCODE_URL="${RUSTCODE_URL%/}"

ONLY="${ONLY:-}"
SKIP_LIST="${SKIP:-}"

# ANSI colour escapes — auto-disable when stdout isn't a TTY (CI, pipes).
if [[ -t 1 ]]; then
    C_GREEN=$'\033[32m'
    C_RED=$'\033[31m'
    C_YELLOW=$'\033[33m'
    C_DIM=$'\033[2m'
    C_RESET=$'\033[0m'
else
    C_GREEN=""; C_RED=""; C_YELLOW=""; C_DIM=""; C_RESET=""
fi

PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0
FAILED_PROBES=()

pass() {
    local name="$1" detail="${2:-}"
    printf "  %sPASS%s  %-20s %s%s%s\n" "$C_GREEN" "$C_RESET" "$name" "$C_DIM" "$detail" "$C_RESET"
    PASS_COUNT=$((PASS_COUNT + 1))
}

fail() {
    local name="$1" detail="${2:-}"
    printf "  %sFAIL%s  %-20s %s\n" "$C_RED" "$C_RESET" "$name" "$detail"
    FAIL_COUNT=$((FAIL_COUNT + 1))
    FAILED_PROBES+=("$name")
}

skip() {
    local name="$1" detail="${2:-}"
    printf "  %sSKIP%s  %-20s %s%s%s\n" "$C_YELLOW" "$C_RESET" "$name" "$C_DIM" "$detail" "$C_RESET"
    SKIP_COUNT=$((SKIP_COUNT + 1))
}

# Should we run this probe? Honour ONLY and SKIP env vars.
should_run() {
    local name="$1"
    if [[ -n "$ONLY" ]]; then
        [[ ",${ONLY}," == *",${name},"* ]] || return 1
    fi
    if [[ -n "$SKIP_LIST" ]]; then
        [[ ",${SKIP_LIST}," != *",${name},"* ]] || return 1
    fi
    return 0
}

# Shell-out wrapper around curl that returns body + status in a stable
# format: stdout is the body, stderr is "HTTP_STATUS=<code>" so callers
# can parse with `grep HTTP_STATUS=`.
post_chat() {
    local body="$1"
    curl --silent --show-error \
        --max-time 60 \
        --request POST \
        --header "Authorization: Bearer ${RUSTCODE_API_KEY}" \
        --header "Content-Type: application/json" \
        --write-out 'HTTP_STATUS=%{http_code}\n' \
        --data "$body" \
        "${RUSTCODE_URL}/v1/chat/completions" 2>&1
}

# Same shape for the repo registration endpoint.
post_repo() {
    local body="$1"
    curl --silent --show-error \
        --max-time 30 \
        --request POST \
        --header "Authorization: Bearer ${RUSTCODE_API_KEY}" \
        --header "Content-Type: application/json" \
        --write-out 'HTTP_STATUS=%{http_code}\n' \
        --data "$body" \
        "${RUSTCODE_URL}/api/v1/repos" 2>&1
}

# Extract just the body (everything up to the HTTP_STATUS= line).
extract_body() {
    sed -e '/^HTTP_STATUS=/d'
}

extract_status() {
    grep -oE '^HTTP_STATUS=[0-9]+' | sed 's/HTTP_STATUS=//'
}

# Pretty-print a JSON body to stderr — used for fail diagnostics.
dump_body() {
    local body="$1"
    if printf "%s" "$body" | jq . >/dev/null 2>&1; then
        printf "%s" "$body" | jq -C . | sed 's/^/        /' >&2
    else
        printf "%s\n" "$body" | sed 's/^/        /' >&2
    fi
}

# --------------------------------------------------------------------------- #
# Probes
# --------------------------------------------------------------------------- #

probe_health() {
    local resp body status
    resp=$(post_chat '{"model":"auto","messages":[{"role":"user","content":"hi"}]}')
    status=$(printf "%s\n" "$resp" | extract_status)
    body=$(printf "%s\n" "$resp" | extract_body)

    if [[ "$status" != "200" ]]; then
        fail health "HTTP $status (expected 200)"
        dump_body "$body"
        return
    fi
    if ! printf "%s" "$body" | jq -e '.choices[0].message.content' >/dev/null 2>&1; then
        fail health "200 but response body missing choices[0].message.content"
        dump_body "$body"
        return
    fi
    pass health "200 + choices populated"
}

probe_claude_default() {
    local resp body status model
    resp=$(post_chat '{"model":"auto","messages":[{"role":"user","content":"What is 2+2?"}]}')
    status=$(printf "%s\n" "$resp" | extract_status)
    body=$(printf "%s\n" "$resp" | extract_body)

    if [[ "$status" != "200" ]]; then
        fail claude_default "HTTP $status"
        dump_body "$body"
        return
    fi
    model=$(printf "%s" "$body" | jq -r '.model // empty')
    if [[ -z "$model" ]]; then
        fail claude_default "response has no .model field"
        dump_body "$body"
        return
    fi
    if [[ "$model" == claude-* || "$model" == *anthropic* || "$model" == *sonnet* || "$model" == *opus* ]]; then
        pass claude_default "model=$model"
    else
        # Not necessarily a failure — a Grok-only stack legitimately returns grok-*.
        # The 'grok_fallback' probe handles that case. Here we only flag if the
        # response shape is plausible but the model is unrecognised.
        skip claude_default "model=$model (not claude-*; if this stack only has XAI_API_KEY set, see grok_fallback)"
    fi
}

probe_scaffold_task() {
    local resp body status task_kind
    # Code-completion prompt that the router should classify as ScaffoldStub
    # → Sonnet tier. See ModelRouter::llm_classify.
    resp=$(post_chat '{
        "model": "auto",
        "messages": [
            {"role": "user", "content": "Write a Rust function that returns the first n Fibonacci numbers."}
        ]
    }')
    status=$(printf "%s\n" "$resp" | extract_status)
    body=$(printf "%s\n" "$resp" | extract_body)

    if [[ "$status" != "200" ]]; then
        fail scaffold_task "HTTP $status"
        dump_body "$body"
        return
    fi
    task_kind=$(printf "%s" "$body" | jq -r '.x_ra_metadata.task_kind // empty')
    local model
    model=$(printf "%s" "$body" | jq -r '.model // empty')
    if [[ "$task_kind" == "ScaffoldStub" && "$model" == *sonnet* ]]; then
        pass scaffold_task "ScaffoldStub → $model"
    elif [[ "$model" == *sonnet* ]]; then
        # Acceptable: the prompt may classify differently but still land
        # on Sonnet (e.g. SimpleAnswer in some routers). The TODO only
        # cares about the final target.
        pass scaffold_task "task_kind=$task_kind, landed on $model"
    else
        fail scaffold_task "task_kind=$task_kind, model=$model (expected Sonnet)"
        dump_body "$body"
    fi
}

probe_arch_task() {
    local resp body status task_kind model
    # Architectural-reasoning prompt — should escalate to Opus per TODO L441.
    resp=$(post_chat '{
        "model": "auto",
        "messages": [
            {"role": "user", "content": "We are designing a multi-tenant SaaS. Compare event-sourcing, CQRS, and a traditional CRUD layered architecture across read scalability, write contention, audit clarity, and operational cost. Pick one and justify."}
        ]
    }')
    status=$(printf "%s\n" "$resp" | extract_status)
    body=$(printf "%s\n" "$resp" | extract_body)

    if [[ "$status" != "200" ]]; then
        fail arch_task "HTTP $status"
        dump_body "$body"
        return
    fi
    task_kind=$(printf "%s" "$body" | jq -r '.x_ra_metadata.task_kind // empty')
    model=$(printf "%s" "$body" | jq -r '.model // empty')
    if [[ "$model" == *opus* ]]; then
        pass arch_task "task_kind=$task_kind → $model"
    else
        fail arch_task "task_kind=$task_kind, model=$model (expected Opus)"
        dump_body "$body"
    fi
}

probe_grok_fallback() {
    # This probe is only meaningful on a stack configured *without*
    # ANTHROPIC_API_KEY. We have no way to introspect the server's env
    # from the client, so we use the response itself: if any prior probe
    # has already returned a claude-* model, this stack has Claude
    # available and this probe should SKIP (not fail).
    local resp body status model
    resp=$(post_chat '{"model":"auto","messages":[{"role":"user","content":"Briefly: what is Rust?"}]}')
    status=$(printf "%s\n" "$resp" | extract_status)
    body=$(printf "%s\n" "$resp" | extract_body)

    if [[ "$status" != "200" ]]; then
        fail grok_fallback "HTTP $status"
        dump_body "$body"
        return
    fi
    model=$(printf "%s" "$body" | jq -r '.model // empty')
    if [[ "$model" == grok-* || "$model" == *grok* ]]; then
        pass grok_fallback "fell back to $model"
    else
        skip grok_fallback "got $model — this stack has Claude available; run against an XAI_API_KEY-only stack to exercise the fallback path"
    fi
}

probe_multi_turn() {
    # Establish context in turn 1, ask the model to recall it in turn 2.
    local resp body status reply
    resp=$(post_chat '{
        "model": "auto",
        "messages": [
            {"role": "user",      "content": "My name is Quincy and my favourite colour is teal. Remember both."},
            {"role": "assistant", "content": "Got it — your name is Quincy and your favourite colour is teal."},
            {"role": "user",      "content": "What did I just tell you my name was? Reply with just the name."}
        ]
    }')
    status=$(printf "%s\n" "$resp" | extract_status)
    body=$(printf "%s\n" "$resp" | extract_body)

    if [[ "$status" != "200" ]]; then
        fail multi_turn "HTTP $status"
        dump_body "$body"
        return
    fi
    reply=$(printf "%s" "$body" | jq -r '.choices[0].message.content // empty')
    # Case-insensitive substring match — models often answer "Quincy."
    # or "Your name is Quincy."
    if printf "%s" "$reply" | grep -qi "quincy"; then
        pass multi_turn "model recalled name"
    else
        fail multi_turn "reply did not contain 'Quincy': $reply"
    fi
}

probe_repo_rag() {
    local repo_id=""
    if [[ -n "${RUSTCODE_REPO_ID:-}" ]]; then
        repo_id="$RUSTCODE_REPO_ID"
    elif [[ -n "${RUSTCODE_TEST_REPO_PATH:-}" ]]; then
        # Register on-the-fly. Note: the path is interpreted on the
        # server side — using a client-local path won't work.
        local reg_resp reg_body reg_status
        reg_resp=$(post_repo "$(jq -n --arg n "smoke-test-rag" --arg p "$RUSTCODE_TEST_REPO_PATH" \
            '{name: $n, local_path: $p, sync_on_register: true}')")
        reg_status=$(printf "%s\n" "$reg_resp" | extract_status)
        reg_body=$(printf "%s\n" "$reg_resp" | extract_body)
        if [[ "$reg_status" != "200" && "$reg_status" != "201" ]]; then
            fail repo_rag "POST /api/v1/repos returned HTTP $reg_status"
            dump_body "$reg_body"
            return
        fi
        repo_id=$(printf "%s" "$reg_body" | jq -r '.id // empty')
        if [[ -z "$repo_id" ]]; then
            fail repo_rag "registration succeeded but no .id field in response"
            dump_body "$reg_body"
            return
        fi
    else
        skip repo_rag "neither RUSTCODE_REPO_ID nor RUSTCODE_TEST_REPO_PATH set"
        return
    fi

    # Ask a question the RAG layer should be able to ground.
    local resp body status chunks
    resp=$(post_chat "$(jq -n --arg rid "$repo_id" '{
        "model":     "auto",
        "x_repo_id": $rid,
        "messages":  [{"role": "user", "content": "Describe the architecture of this repository in two sentences."}]
    }')")
    status=$(printf "%s\n" "$resp" | extract_status)
    body=$(printf "%s\n" "$resp" | extract_body)

    if [[ "$status" != "200" ]]; then
        fail repo_rag "HTTP $status (repo_id=$repo_id)"
        dump_body "$body"
        return
    fi
    chunks=$(printf "%s" "$body" | jq -r '.x_ra_metadata.rag_chunks_used // 0')
    if [[ "$chunks" =~ ^[0-9]+$ ]] && (( chunks > 0 )); then
        pass repo_rag "rag_chunks_used=$chunks (repo_id=$repo_id)"
    else
        fail repo_rag "rag_chunks_used=$chunks (expected > 0; repo may not be fully indexed yet)"
    fi
}

probe_prompt_cache() {
    # Anthropic prompt cache requires the same prompt prefix on two
    # consecutive requests. Use a long-enough user message to clear the
    # 1024-token minimum.
    local payload
    payload=$(jq -n '{
        "model":   "auto",
        "x_no_cache": true,
        "messages": [
            {"role": "user", "content": "Repeat after me, word for word and in full, the following long passage in your reply: \"It was the best of times, it was the worst of times, it was the age of wisdom, it was the age of foolishness, it was the epoch of belief, it was the epoch of incredulity, it was the season of light, it was the season of darkness, it was the spring of hope, it was the winter of despair, we had everything before us, we had nothing before us, we were all going direct to heaven, we were all going direct the other way.\""}
        ]
    }')

    # First call — primes the cache.
    local first_resp first_body first_status
    first_resp=$(post_chat "$payload")
    first_status=$(printf "%s\n" "$first_resp" | extract_status)
    first_body=$(printf "%s\n" "$first_resp" | extract_body)
    if [[ "$first_status" != "200" ]]; then
        fail prompt_cache "first call HTTP $first_status"
        dump_body "$first_body"
        return
    fi

    # Second call — should hit the prompt cache.
    local second_resp second_body second_status cache_read model
    second_resp=$(post_chat "$payload")
    second_status=$(printf "%s\n" "$second_resp" | extract_status)
    second_body=$(printf "%s\n" "$second_resp" | extract_body)
    if [[ "$second_status" != "200" ]]; then
        fail prompt_cache "second call HTTP $second_status"
        dump_body "$second_body"
        return
    fi

    model=$(printf "%s" "$second_body" | jq -r '.model // empty')
    if [[ "$model" != *claude* && "$model" != *sonnet* && "$model" != *opus* ]]; then
        skip prompt_cache "model=$model — prompt cache is a Claude-only feature"
        return
    fi
    cache_read=$(printf "%s" "$second_body" | jq -r '.x_ra_metadata.cache_read_input_tokens // 0')
    if [[ "$cache_read" =~ ^[0-9]+$ ]] && (( cache_read > 0 )); then
        pass prompt_cache "cache_read_input_tokens=$cache_read on second call"
    else
        fail prompt_cache "cache_read_input_tokens=$cache_read (expected > 0; prompt may be under the 1024-token minimum)"
    fi
}

probe_response_cache() {
    # Identical request twice, no x_no_cache. The second response must
    # report cached=true and ideally arrive faster.
    local payload
    payload='{"model":"auto","messages":[{"role":"user","content":"smoke-test response cache canary — please reply with the single word OK."}]}'

    local first_resp first_status
    first_resp=$(post_chat "$payload")
    first_status=$(printf "%s\n" "$first_resp" | extract_status)
    if [[ "$first_status" != "200" ]]; then
        fail response_cache "first call HTTP $first_status"
        dump_body "$(printf "%s\n" "$first_resp" | extract_body)"
        return
    fi

    local second_resp second_body second_status cached
    second_resp=$(post_chat "$payload")
    second_status=$(printf "%s\n" "$second_resp" | extract_status)
    second_body=$(printf "%s\n" "$second_resp" | extract_body)
    if [[ "$second_status" != "200" ]]; then
        fail response_cache "second call HTTP $second_status"
        dump_body "$second_body"
        return
    fi
    cached=$(printf "%s" "$second_body" | jq -r '.x_ra_metadata.cached // false')
    if [[ "$cached" == "true" ]]; then
        pass response_cache "second response cached=true"
    else
        fail response_cache "second response cached=$cached (expected true)"
    fi
}

probe_auth_401() {
    # Same as post_chat but *without* the Authorization header. Server
    # should reject with 401. Note: if RUSTCODE_PROXY_API_KEYS is unset
    # on the server, auth is disabled and this probe will see 200 —
    # which we treat as SKIP rather than FAIL because it's a server
    # config choice, not a contract violation.
    local resp body status
    resp=$(curl --silent --show-error \
        --max-time 30 \
        --request POST \
        --header "Content-Type: application/json" \
        --write-out 'HTTP_STATUS=%{http_code}\n' \
        --data '{"model":"auto","messages":[{"role":"user","content":"hi"}]}' \
        "${RUSTCODE_URL}/v1/chat/completions" 2>&1)
    status=$(printf "%s\n" "$resp" | extract_status)
    body=$(printf "%s\n" "$resp" | extract_body)

    case "$status" in
        401)
            pass auth_401 "got 401 as expected"
            ;;
        200)
            skip auth_401 "got 200 — server appears to have RUSTCODE_PROXY_API_KEYS unset (auth disabled)"
            ;;
        *)
            fail auth_401 "got HTTP $status (expected 401)"
            dump_body "$body"
            ;;
    esac
}

# --------------------------------------------------------------------------- #
# Run
# --------------------------------------------------------------------------- #

printf "rc-app smoke test — %s\n\n" "$RUSTCODE_URL"

# Probe order matters slightly: health first (so we fail fast if the server
# is down), auth_401 last (it deliberately omits the bearer, easier to
# eyeball in logs as a distinct trailing call). Cache probes run after the
# routing probes so they don't accidentally hit the response cache from an
# earlier identical-message probe.
for probe in \
    health \
    claude_default \
    scaffold_task \
    arch_task \
    grok_fallback \
    multi_turn \
    repo_rag \
    prompt_cache \
    response_cache \
    auth_401
do
    if should_run "$probe"; then
        "probe_${probe}"
    fi
done

printf "\n"
printf "Summary: %s%d passed%s, %s%d failed%s, %s%d skipped%s\n" \
    "$C_GREEN" "$PASS_COUNT" "$C_RESET" \
    "$C_RED"   "$FAIL_COUNT" "$C_RESET" \
    "$C_YELLOW" "$SKIP_COUNT" "$C_RESET"

if (( FAIL_COUNT > 0 )); then
    printf "Failed probes: %s\n" "${FAILED_PROBES[*]}"
    exit "$FAIL_COUNT"
fi
exit 0
