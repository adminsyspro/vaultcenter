# syntax=docker/dockerfile:1
FROM node:26.2.0-bookworm-slim AS ui
WORKDIR /src/ui
ENV NEXT_TELEMETRY_DISABLED=1
COPY ui/package.json ui/package-lock.json ./
RUN npm ci
COPY ui/ ./
RUN npm run build

FROM rust:1.96.1-bookworm AS rust
ARG REVISION=unknown
ENV CARGO_BUILD_JOBS=2 \
    VAULTCENTER_REVISION=$REVISION
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake clang pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY deploy/cold-vtl/vtl_runtime.py deploy/cold-vtl/vtl_runtime.py
COPY ui/public/brand/vaultcenter.png ui/public/brand/vaultcenter.png
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --locked --workspace

FROM debian:bookworm-slim AS mhvtl
ARG MHVTL_REVISION=59f32ee50b3269c118965bffa29cf38b1553268f
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates git build-essential zlib1g-dev liblzo2-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src/mhvtl
RUN git init && git remote add origin https://github.com/markh794/mhvtl.git \
    && git fetch --depth=1 origin "$MHVTL_REVISION" && git checkout --detach FETCH_HEAD \
    && test "$(git rev-parse HEAD)" = "$MHVTL_REVISION"
COPY deploy/mhvtl/nonblocking-status-fifo.patch /tmp/nonblocking-status-fifo.patch
COPY deploy/mhvtl/telemetry-heartbeat.patch /tmp/telemetry-heartbeat.patch
COPY deploy/mhvtl/bandwidth.patch /tmp/bandwidth.patch
COPY deploy/mhvtl/parallel-host-queue.patch /tmp/parallel-host-queue.patch
COPY deploy/mhvtl/online-cartridges.patch /tmp/online-cartridges.patch
COPY deploy/mhvtl/command-wakeup.patch /tmp/command-wakeup.patch
COPY deploy/mhvtl/media-lock.patch /tmp/media-lock.patch
COPY deploy/mhvtl/control-extension.patch /tmp/control-extension.patch
COPY deploy/mhvtl/slot-map.patch /tmp/slot-map.patch
COPY deploy/mhvtl/source-archive.sh /tmp/source-archive.sh
RUN git apply /tmp/nonblocking-status-fifo.patch && git apply /tmp/telemetry-heartbeat.patch && git apply /tmp/bandwidth.patch && git apply /tmp/parallel-host-queue.patch && git apply /tmp/online-cartridges.patch && git apply /tmp/command-wakeup.patch && git apply /tmp/media-lock.patch && git apply /tmp/control-extension.patch && git apply /tmp/slot-map.patch
# The sources DKMS compiles on the customer's host. Archived before anything is
# built, by the script the CI job that compiles the module uses too: the sha256
# of the archive names the module version, it must not change with the build.
RUN mkdir -p /stage/usr/share/doc/mhvtl \
    && sh /tmp/source-archive.sh /src/mhvtl /stage/usr/share/doc/mhvtl/source.tar.gz
# Userspace only. Never build a module against the CI runner's kernel.
RUN make -j2 usr etc \
    && make -C usr install DESTDIR=/stage ROOTUID=NO LIBDIR=/usr/lib SYSTEMD_GENERATOR_DIR=/usr/lib/systemd/system-generators \
    && make -C etc install DESTDIR=/stage ROOTUID=NO SYSTEMD_SERVICE_DIR=/usr/lib/systemd/system \
    && rm -rf /stage/etc/mhvtl

FROM debian:bookworm-slim AS appliance
ARG VERSION=dev
ARG REVISION=unknown
LABEL org.opencontainers.image.title="VaultCenter" \
    org.opencontainers.image.source="https://github.com/adminsyspro/vaultcenter" \
    org.opencontainers.image.version="$VERSION" \
    org.opencontainers.image.revision="$REVISION" \
    org.opencontainers.image.description="mhVTL/iSCSI appliance; requires a dedicated Linux host with the mhvtl kernel module"
ENV container=docker
RUN printf '#!/bin/sh\nexit 101\n' > /usr/sbin/policy-rc.d \
    && chmod 755 /usr/sbin/policy-rc.d \
    && apt-get update && apt-get install -y --no-install-recommends \
       systemd systemd-sysv dbus ca-certificates curl python3 openssh-client \
       procps tgt sg3-utils lsscsi mtx mt-st kmod zlib1g liblzo2-2 libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && rm -f /etc/machine-id && touch /etc/machine-id \
    && systemctl mask systemd-udevd.service systemd-udevd-control.socket \
       systemd-udevd-kernel.socket systemd-modules-load.service getty.target
COPY --from=mhvtl /stage/ /
COPY --from=rust /src/target/release/cold-vtld /src/target/release/cold-vtl /src/target/release/cold-vtl-worker /usr/local/bin/
COPY --from=ui /src/ui/out/ /usr/local/share/vaultcenter/ui/
COPY deploy/cold-vtl/cold-vtld.service deploy/cold-vtl/vaultcenter-telemetry.service /etc/systemd/system/
COPY deploy/cold-vtl/telemetry.py /usr/local/libexec/vaultcenter-telemetry
COPY deploy/cold-vtl/pbs_inventory.py /usr/local/libexec/pbs_inventory.py
COPY deploy/mhvtl/telemetry-pid.conf /etc/systemd/system/vtltape@.service.d/telemetry-pid.conf
COPY deploy/mhvtl/telemetry-tmpfiles.conf /usr/lib/tmpfiles.d/vaultcenter-telemetry.conf
COPY deploy/docker/systemd/container.conf /etc/systemd/system/cold-vtld.service.d/container.conf
COPY deploy/docker/systemd/host-modules.conf /etc/systemd/system/mhvtl-load-modules.service.d/host-modules.conf
COPY deploy/docker/systemd/register-luns.conf /etc/systemd/system/vtllibrary@.service.d/register-luns.conf
COPY deploy/docker/systemd/register-luns.conf /etc/systemd/system/vtltape@.service.d/register-luns.conf
COPY deploy/mhvtl/tgt.service.d/cold-vtl.conf /etc/systemd/system/tgt.service.d/cold-vtl.conf
COPY deploy/mhvtl/device.conf deploy/mhvtl/mhvtl.conf deploy/docker/library_contents.10 /etc/mhvtl/
COPY --chmod=755 deploy/cold-vtl/check-quiescent /usr/local/libexec/cold-vtl-check-quiescent
COPY --chmod=755 deploy/cold-vtl/maintenance-controller.py /usr/local/libexec/cold-vtl-maintenance
COPY --chmod=755 deploy/cold-vtl/cartridge-controller.py /usr/local/libexec/cold-vtl-cartridges
COPY deploy/cold-vtl/export_readiness.py /usr/local/libexec/export_readiness.py
COPY --chmod=755 deploy/cold-vtl/reintegration-controller.py /usr/local/libexec/cold-vtl-reintegration
COPY deploy/cold-vtl/recovery_install.py /usr/local/libexec/recovery_install.py
COPY deploy/cold-vtl/cartridge_deletion.py deploy/cold-vtl/cartridge_online.py deploy/cold-vtl/cartridge_batches.py deploy/cold-vtl/vtl_runtime.py deploy/cold-vtl/bandwidth.py deploy/cold-vtl/pbs_connection.py deploy/cold-vtl/client_binding.py deploy/cold-vtl/client_lease.py deploy/docker/isolated-boot.py /usr/local/libexec/
COPY --chmod=755 deploy/cold-vtl/vtl-controller.py /usr/local/libexec/cold-vtl-vtl
COPY --chmod=755 deploy/cold-vtl/worker-run.py /usr/local/libexec/cold-vtl-worker-run
COPY --chmod=755 deploy/cold-vtl/storage-check.py /usr/local/libexec/storage-check.py
COPY deploy/docker/systemd/storage-condition.conf /tmp/storage-condition.conf
COPY --chmod=755 deploy/cold-vtl/notify-email.py /usr/local/libexec/vaultcenter-notify-email
COPY --chmod=755 deploy/cold-vtl/archive-controller.py /usr/local/libexec/cold-vtl-archive
COPY --chmod=755 deploy/mhvtl/cold-vtl-wait-devices /usr/local/sbin/cold-vtl-wait-devices
COPY deploy/docker/preflight.py /usr/local/libexec/vaultcenter-preflight.py
COPY --chmod=755 deploy/docker/entrypoint.sh deploy/docker/healthcheck.sh /usr/local/bin/
COPY --chmod=755 deploy/install/install.sh deploy/install/prepare-dkms-source.sh /usr/local/share/vaultcenter/host/
COPY deploy/install/dkms.conf deploy/host/install.py deploy/host/storage_controller.py deploy/host/storage_paths.py deploy/host/vtl_state.py deploy/host/vtl_storage.py deploy/host/storage_mounts.py deploy/host/vaultcenter-storage-host.service deploy/host/vaultcenter-storage-mounts.service deploy/host/vaultcenter-mhvtl.service deploy/host/vaultcenter-mhvtl.conf deploy/host/99-vaultcenter-console.conf deploy/docker/99-vaultcenter.rules deploy/docker/compose.yaml deploy/docker/auth.env.example deploy/host/update.py /usr/local/share/vaultcenter/host/
# The guide the messages of install.sh and update.py point to: on the host it is
# /usr/local/share/vaultcenter/<version>/host/install.md.
COPY docs/install.md /usr/local/share/vaultcenter/host/
# The updater checks the isolation receipt against this vtl_runtime.py. The
# receipt is shipped only by a version that changes it: the bracket glob lets
# COPY succeed when the file is absent.
COPY deploy/cold-vtl/vtl_runtime.py deploy/cold-vtl/isolation-qualified.jso[n] /usr/local/share/vaultcenter/host/
COPY deploy/cold-vtl/maintenance-condition.conf /tmp/maintenance-condition.conf
RUN ldconfig \
    && mkdir -p /var/lib/cold-vtl/restores /var/lib/vaultcenter-media /opt/mhvtl /etc/cold-vtl \
    && chmod 700 /var/lib/cold-vtl \
    && for unit in mhvtl.target vtllibrary@10.service vtltape@11.service tgt.service; do \
         mkdir -p "/etc/systemd/system/$unit.d"; \
         cp /tmp/maintenance-condition.conf "/etc/systemd/system/$unit.d/maintenance.conf"; \
       done \
    && for unit in vtllibrary@10.service vtltape@11.service tgt.service; do \
         cp /tmp/storage-condition.conf "/etc/systemd/system/$unit.d/storage.conf"; \
       done \
    && rm /tmp/storage-condition.conf \
    && rm /tmp/maintenance-condition.conf \
    && systemctl enable vaultcenter-telemetry.service cold-vtld.service mhvtl.target vtllibrary@10.service vtltape@11.service tgt.service
EXPOSE 8443 3260
STOPSIGNAL SIGRTMIN+3
HEALTHCHECK --interval=30s --timeout=10s --start-period=60s --retries=3 CMD ["/usr/local/bin/healthcheck.sh"]
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
