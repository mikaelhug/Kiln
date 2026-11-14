FROM rust:1.99.1-alpine AS build
RUN apk add --no-cache musl-dev gcc g++ make cmake clang perl linux-headers ca-certificates
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p kiln \
    && cp target/release/kiln /kiln \
    && mkdir -m 1777 /scratch-tmp

FROM scratch AS binary
COPY --from=build /kiln /kiln

FROM scratch
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /kiln /kiln
COPY --from=build /scratch-tmp /tmp
ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
USER 10001:10001
ENTRYPOINT ["/kiln"]
CMD ["serve"]
