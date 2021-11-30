FROM rust:1.56.0-slim-bullseye as build-env
WORKDIR /app
COPY . /app
RUN cargo build --release

FROM gcr.io/distroless/cc-debian11
COPY --from=build-env /app/target/release/ingress-nginx-errors /
ENTRYPOINT ["/ingress-nginx-errors"]
