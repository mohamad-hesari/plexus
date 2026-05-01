FROM rust:1.95-slim AS builder

WORKDIR /app

# Copy the source code and configuration
COPY . .

# Build the application in release mode
RUN cargo build --release

FROM scratch AS export-stage
COPY --from=builder /app/target/release/plexus /
