#!/bin/sh
set -e

# Install protoc 28.3 (libsignal v0.97.0 proto3 requires --experimental_allow_proto3_optional)
cd /tmp
apt-get update && apt-get install -y curl unzip perl
curl --retry 5 --retry-delay 10 -sLO https://github.com/protocolbuffers/protobuf/releases/download/v28.3/protoc-28.3-linux-x86_64.zip
unzip -q -o protoc-28.3-linux-x86_64.zip -d /usr/local
chmod +x /usr/local/bin/protoc
rm protoc-28.3-linux-x86_64.zip
echo "protoc installed: $(protoc --version)"

# Build OpenSSL for aarch64-musl target (needed by libsqlite3-sys/sqlcipher)
OPENSSL_VERSION=3.0.15
curl -sLO "https://github.com/openssl/openssl/releases/download/openssl-${OPENSSL_VERSION}/openssl-${OPENSSL_VERSION}.tar.gz"
tar xzf openssl-${OPENSSL_VERSION}.tar.gz
cd openssl-${OPENSSL_VERSION}
CC=aarch64-linux-musl-gcc \
AR=aarch64-linux-musl-ar \
RANLIB=aarch64-linux-musl-ranlib \
./Configure linux-aarch64 \
  --prefix=/usr/local/aarch64-musl \
  --openssldir=/usr/local/aarch64-musl/ssl \
  no-shared no-asm no-tests \
  -DOPENSSL_NO_SECURE_MEMORY
make -j$(nproc)
make install_sw
cd /tmp
rm -rf openssl-${OPENSSL_VERSION}*

# Persist env vars for cargo build via cargo config
mkdir -p /root/.cargo
cat > /root/.cargo/config.toml << 'EOF'
[env]
OPENSSL_DIR = "/usr/local/aarch64-musl"
OPENSSL_LIB_DIR = "/usr/local/aarch64-musl/lib"
OPENSSL_INCLUDE_DIR = "/usr/local/aarch64-musl/include"
AARCH64_UNKNOWN_LINUX_MUSL_OPENSSL_DIR = "/usr/local/aarch64-musl"
PKG_CONFIG_ALLOW_CROSS = "1"
EOF

echo "OpenSSL cross-build done for aarch64-musl"
