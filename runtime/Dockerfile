FROM rust:1.95.0-bookworm@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1 AS buzz-builder

ADD --checksum=sha256:ad8954e1ea138acc39ab781f4d83ea048ba8d5cf31e3060f361a691ce819c2c5 https://codeload.github.com/block/buzz/tar.gz/9ceb1f79bbc21785a0a075c40aecb3c058b1ea15 /tmp/buzz.tar.gz
RUN mkdir -p /src/buzz \
    && tar -xzf /tmp/buzz.tar.gz -C /src/buzz --strip-components=1 \
    && cd /src/buzz \
    && cargo build --release --locked -p buzz-cli

FROM nousresearch/hermes-agent:v2026.9.11

RUN apt-get update \
    && apt-get install -y --no-install-recommends tesseract-ocr tesseract-ocr-chi-sim tesseract-ocr-eng \
    && rm -rf /var/lib/apt/lists/*
RUN uv pip install --python /opt/hermes/.venv/bin/python --no-cache \
    pypdfium2==5.13.0 python-docx==1.2.0 openpyxl==3.1.5 python-pptx==1.0.2 defusedxml==0.7.1

RUN install -d -o root -g root -m 0555 /opt/fork /opt/fork/secrets
COPY --from=buzz-builder --chown=root:root --chmod=0555 /src/buzz/target/release/buzz /usr/local/bin/buzz
COPY --chown=root:root --chmod=0555 plugins/fork_knowledge /opt/hermes/plugins/fork_knowledge
COPY --chown=root:root --chmod=0444 harden_buzz_gateway.py /opt/fork/harden_buzz_gateway.py
COPY --chown=root:root --chmod=0444 harden_memory.py /opt/fork/harden_memory.py
COPY --chown=root:root --chmod=0444 fork_memory_scope.py /opt/hermes/tools/fork_memory_scope.py
COPY --chown=root:root --chmod=0444 config.base.json /opt/fork/config.base.json
COPY --chown=root:root --chmod=0444 merge_config.py /opt/fork/merge_config.py
COPY --chown=root:root --chmod=0555 entrypoint.sh /opt/fork/entrypoint.sh

RUN /opt/hermes/.venv/bin/python /opt/fork/harden_buzz_gateway.py /opt/hermes/plugins/platforms/buzz/adapter.py
RUN /opt/hermes/.venv/bin/python /opt/fork/harden_memory.py \
    /opt/hermes/tools/memory_tool_store.py /opt/hermes/agent/agent_init.py /opt/hermes/gateway/run_turn_runner.py \
    && /opt/hermes/.venv/bin/python /opt/hermes/tools/fork_memory_scope.py
