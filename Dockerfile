FROM rustlang/rust:nightly as build
WORKDIR /usr/src

# Install musl-gcc
RUN apt-get update && apt-get install -y musl-tools

# Download the target for static linking.
RUN rustup target add x86_64-unknown-linux-musl

# Copy the source and build the application.
COPY . ./
RUN cargo install --root . --bin constellation --target x86_64-unknown-linux-musl --path .

# Copy the statically-linked binary into a scratch container.
FROM scratch
COPY --from=build /usr/src/bin/constellation .
USER 1000
ENTRYPOINT ["./constellation"]
