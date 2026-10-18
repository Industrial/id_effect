# ClusterFuzzLite build image for the cargo-fuzz targets in fuzz/ (docs/security/fuzzing.md).
# base-builder-rust v1, pinned by digest.
FROM gcr.io/oss-fuzz-base/base-builder-rust:v1@sha256:3428d0a90b1e7efbe2ba947bd1d0c9a70594986b4b6553804547f1fb24cfc92f
# The image pins its own nightly through RUSTUP_TOOLCHAIN; build on the one fuzz/rust-toolchain.toml
# names instead (0.11.0.md D20). Bump the two together.
ENV RUSTUP_TOOLCHAIN=nightly-2026-07-10
RUN rustup toolchain install "$RUSTUP_TOOLCHAIN" --profile minimal
COPY . $SRC/id_effect
WORKDIR $SRC/id_effect
COPY .clusterfuzzlite/build.sh $SRC/
