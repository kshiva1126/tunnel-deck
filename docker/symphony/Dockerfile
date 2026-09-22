FROM rust:1.85.0-bookworm

ARG CODEX_VERSION=0.155.1
ARG GH_VERSION=2.101.0
ARG SYMPHONY_VERSION=0.0.3
ARG WORKER_UID=1000
ARG WORKER_GID=1000

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl git jq nodejs npm socat \
    && rm -rf /var/lib/apt/lists/* \
    && npm install --global "@openai/codex@${CODEX_VERSION}" \
    && rustup component add rustfmt clippy \
    && curl -fsSLo /tmp/symphony \
      "https://github.com/openai/symphony/releases/download/v${SYMPHONY_VERSION}/symphony-v${SYMPHONY_VERSION}-linux_x86_64" \
    && curl -fsSLo /tmp/symphony.sha256 \
      "https://github.com/openai/symphony/releases/download/v${SYMPHONY_VERSION}/symphony-v${SYMPHONY_VERSION}-linux_x86_64.sha256" \
    && sed "s/symphony-v${SYMPHONY_VERSION}-linux_x86_64/symphony/" /tmp/symphony.sha256 \
      | (cd /tmp && sha256sum --check -) \
    && install -m 0755 /tmp/symphony /usr/local/bin/symphony \
    && curl -fsSLo /tmp/gh.tar.gz \
      "https://github.com/cli/cli/releases/download/v${GH_VERSION}/gh_${GH_VERSION}_linux_amd64.tar.gz" \
    && curl -fsSLo /tmp/gh_checksums.txt \
      "https://github.com/cli/cli/releases/download/v${GH_VERSION}/gh_${GH_VERSION}_checksums.txt" \
    && gh_sha256=$(grep " gh_${GH_VERSION}_linux_amd64.tar.gz$" /tmp/gh_checksums.txt | cut -d' ' -f1) \
    && printf '%s  %s\n' "$gh_sha256" /tmp/gh.tar.gz | sha256sum --check - \
    && tar -xzf /tmp/gh.tar.gz -C /tmp \
    && install -m 0755 "/tmp/gh_${GH_VERSION}_linux_amd64/bin/gh" /usr/local/bin/gh \
    && ln -s /usr/local/cargo/bin/cargo /usr/local/bin/cargo \
    && ln -s /usr/local/cargo/bin/cargo-fmt /usr/local/bin/cargo-fmt \
    && ln -s /usr/local/cargo/bin/cargo-clippy /usr/local/bin/cargo-clippy \
    && ln -s /usr/local/cargo/bin/rustc /usr/local/bin/rustc \
    && ln -s /usr/local/cargo/bin/rustfmt /usr/local/bin/rustfmt \
    && ln -s /usr/local/cargo/bin/clippy-driver /usr/local/bin/clippy-driver \
    && rm -rf /tmp/* \
    && groupadd --gid "$WORKER_GID" worker \
    && useradd --uid "$WORKER_UID" --gid "$WORKER_GID" --create-home --shell /bin/sh worker

USER worker
WORKDIR /workspaces

ENV HOME=/home/worker \
    CARGO_HOME=/home/worker/.cargo \
    SYMPHONY_INSTALL_DIR=/tmp/symphony-runtime \
    SYMPHONY_CONTROL_ROOT=/control \
    SYMPHONY_WORKSPACE_ROOT=/workspaces

CMD ["symphony", "/control/WORKFLOW.md", "--logs-root", "/logs", "--port", "4000", "--i-understand-that-this-will-be-running-without-the-usual-guardrails"]
