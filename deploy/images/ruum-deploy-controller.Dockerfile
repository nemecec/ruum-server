# syntax=docker/dockerfile:1.7
# The deploy controller of staging (backlog O-34, decision 0008-2): the Go program of
# deploy/images/deploy-controller, plus kubectl, helm and cosign at the versions and SHA-256 sums
# of deploy/staging/versions.env, the chart charts/ruum and the deploy scripts it runs. Built and
# attested by images.yml like the other images; the platform step installs it by digest.
FROM golang:1.26-alpine AS build
RUN apk add --no-cache bash
WORKDIR /src
COPY deploy/images/deploy-controller ./
RUN go vet ./... && go test -count=1 ./... && \
    CGO_ENABLED=0 go build -trimpath -ldflags='-s -w' -o /out/deploy-controller .

# The base of the staging tools image (TOOLS_BASE_IMAGE of versions.env, pinned by digest;
# tests/run.sh checks that both name the same image).
FROM alpine:3.24.4@sha256:f59abe656b692d901e184be3445e084786602108599d2a51d229c0f5a48c71bf
RUN apk add --no-cache bash coreutils curl jq ca-certificates
COPY deploy/staging/setup.sh deploy/staging/versions.env /tmp/setup/
RUN RUUM_TOOLS_BIN=/usr/local/bin bash /tmp/setup/setup.sh kubectl helm cosign && rm -rf /tmp/setup && \
    kubectl version --client >/dev/null && helm version --short >/dev/null && cosign version >/dev/null 2>&1
COPY --from=build /out/deploy-controller /usr/local/bin/deploy-controller
# The chart and the scripts at their paths in the checkout (deploy-chart.sh finds the chart at
# ../charts/ruum).
COPY deploy/staging/charts/ruum /opt/ruum/deploy/staging/charts/ruum
COPY deploy/staging/ops/deploy-chart.sh deploy/staging/ops/deploy-lock.sh deploy/staging/ops/image-check.sh \
     deploy/staging/ops/release-floor.sh /opt/ruum/deploy/staging/ops/
USER 65532:65532
ENV HOME=/tmp HELM_DRIVER=configmap RUUM_STAGING_WORK=/tmp/work
ENTRYPOINT ["/usr/local/bin/deploy-controller"]
