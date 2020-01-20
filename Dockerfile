FROM rustlang/rust:nightly as build
WORKDIR /usr/src

# Install musl-gcc
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools

# Download the target for static linking.
RUN rustup target add x86_64-unknown-linux-musl

# Create a dummy project and build the app's dependencies.
# If the Cargo.toml and Cargo.lock files have not changed,
# we can use the docker build cache and skip this slow step.
RUN USER=root cargo init --bin && USER=root cargo new --lib constellation-internal && mkdir -p src/bin/constellation && echo 'fn main(){}' > src/bin/constellation/main.rs
COPY Cargo.toml build.rs ./
RUN sed -i '/^###$/q' Cargo.toml
COPY constellation-internal/Cargo.toml ./constellation-internal/
RUN cargo generate-lockfile
RUN cargo build --bins --features kubernetes --target x86_64-unknown-linux-musl --release

# Copy the source and build the application.
COPY . ./
RUN touch ./constellation-internal/src/lib.rs
RUN cargo build --locked --frozen --offline --bin constellation --features kubernetes --target x86_64-unknown-linux-musl --release

# Copy the statically-linked binary into a scratch container.
FROM scratch
COPY --from=build /usr/src/target/x86_64-unknown-linux-musl/release/constellation .
USER 1000
ENTRYPOINT ["./constellation"]
