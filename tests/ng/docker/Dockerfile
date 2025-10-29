FROM katsu AS builder

WORKDIR /katsu

COPY . .

WORKDIR /katsu/tests/ng

RUN katsu -o folder katsu-script-test.yaml

FROM scratch AS target
COPY --from=builder /katsu/tests/ng/katsu-work/chroot /