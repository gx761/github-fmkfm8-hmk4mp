# whistle-rs 容器镜像：多阶段构建，产出精简运行镜像。
FROM rust:1-bookworm AS builder
WORKDIR /src
COPY . .
RUN cargo build --release --bin w2r

FROM debian:bookworm-slim
RUN useradd -m -u 10001 whistle
COPY --from=builder /src/target/release/w2r /usr/local/bin/w2r
USER whistle
# 8899: 代理端口；8900: Web 管理界面。
EXPOSE 8899 8900
ENTRYPOINT ["w2r"]
CMD ["start", "--host", "0.0.0.0"]
