ARG PHANPY_VERSION

FROM docker.io/library/alpine:3.22 AS phanpy

ARG PHANPY_VERSION

RUN apk add --no-cache ca-certificates curl tar \
    && mkdir -p /srv/phanpy \
    && curl --fail --location --silent --show-error \
      "https://github.com/cheeaun/phanpy/releases/download/${PHANPY_VERSION}/phanpy-dist.tar.gz" \
      | tar --extract --gzip --file - --directory /srv/phanpy

FROM docker.io/library/caddy:2

RUN apk add --no-cache curl

COPY --from=phanpy /srv/phanpy /srv/phanpy
