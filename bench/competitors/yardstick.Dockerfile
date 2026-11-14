# The yardstick's rivals in one image (bench/yardstick.py): every tool pinned by version and
# checksum by install_rivals.sh, livegrep built by build_livegrep.sh. Milvus and Ollama for
# claude-context run beside it from claude-context.compose.yml.
#   docker build -f bench/competitors/yardstick.Dockerfile -t scrutate-yardstick .
#   docker run --rm --network host -v "$PWD":/src -v ~/corpora:/corpora scrutate-yardstick \
#     python3 /src/bench/yardstick.py --corpus /corpora/linux --queries ... --out /src/yardstick.json
# ubuntu:24.04 as of 2026-11-14
FROM ubuntu@sha256:2198e4c65076e9cb0b3d695219df667c0e3b0072da64ca3c2648aca17dcecaf5 AS livegrep
COPY bench/competitors/build_livegrep.sh /tmp/
RUN bash /tmp/build_livegrep.sh

FROM ubuntu@sha256:2198e4c65076e9cb0b3d695219df667c0e3b0072da64ca3c2648aca17dcecaf5
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends ca-certificates curl xz-utils zstd git build-essential python3 python3-venv \
    && rm -rf /var/lib/apt/lists/*
COPY bench/competitors/install_rivals.sh /tmp/
RUN bash /tmp/install_rivals.sh /opt/rivals && rm -rf /opt/rivals/src
COPY --from=livegrep /out /opt/livegrep
ENV PATH=/opt/rivals/bin:$PATH ZOEKT_BIN_DIR=/opt/rivals/bin CLAUDE_CONTEXT_DIR=/opt/rivals/claude-context LIVEGREP_BIN_DIR=/opt/livegrep \
    MILVUS_ADDRESS=127.0.0.1:19530
