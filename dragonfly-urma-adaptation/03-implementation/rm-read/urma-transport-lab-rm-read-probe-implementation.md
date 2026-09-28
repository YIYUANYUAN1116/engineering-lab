# urma-transport-lab RM READ probe implementation

Date: 2026-09-17

## Result

An isolated real-provider probe has been implemented on branch
`tcp-urma-file-transfer` in `/home/yuan/workspace/dev/urma-transport-lab`.
Dragonfly production READ remains disabled.

The probe uses two processes on one host and one EID. The parent exports an
RM/RTP Jetty and a READ-only Segment. The child imports both objects, posts
signaled READ WRs through a bounded owner loop, routes raw send-side CQEs by
`user_ctx`, verifies the complete SHA-256, drains owners, unimports, and then
allows the parent to unregister and release its explicit token ID.

Implemented evidence:

- real RM/RTP READ rather than a simulated completion;
- raw CQE status, opcode, completion length, local ID, direction, Jetty flag,
  immediate-data validity, remote-ID validity, and event kind;
- exact-once `user_ctx -> WR owner` retirement;
- `-EBUSY` when unimport is requested with an outstanding READ owner;
- successful unimport after CQE drain;
- source unregister and backing lifetime ordered after peer acknowledgement;
- clean Jetty, JFC, JFCE, context, and liburma teardown;
- provider `max_read_size` and `max_write_size` exposed in the lab capability
  snapshot.

The probe records READ completion opcode and completion length as observations.
It does not treat them as protocol constants because UMDK documents those CQE
fields primarily for receive completions.

## Files

- `src/bin/rm_read_probe.rs`
- `src/rm_read_probe.rs`
- `src/ffi/shim.[ch]`
- `src/ffi/mod.rs`
- `tests/rm_read_real_provider.rs`
- `docs/rm-read-real-provider-probe.md`

## Offline verification

- `cargo test rm_read_probe --lib`: passed, 2 tests.
- feature-on `cargo check --all-targets --features urma`: passed against the
  UMDK source headers.
- C shim compilation with those headers: passed as part of the feature-on
  check.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

The full feature-off suite reached 87 passing tests; three existing TCP tests
failed because the execution sandbox rejected their local socket operation with
`EPERM`. The failures are outside the RM READ changes.

## Real-provider result

The corrected probe passed on the B7 provider on 2026-09-17. The run used
`udmac0d1e2`, EID index 1, RM/RTP, a 64 MiB source, 1 MiB READ slices, and depth
128. Child observed 64 successful exact-once CQEs and reported:

- `maxReadSize=268435456`;
- `busyUnimportStatus=-16`;
- `opcodes=[0]`, `completionLengths=[0]`, `localIds=[1050]`;
- `isRecv=false`, `userCtxValid=true`, `isJettyValues=[true]`;
- `remoteIdValidValues=[false]`, `immDataValidValues=[false]`;
- content, owner retirement, and clean shutdown all passed.

Parent also completed source unregister/token release and clean shutdown. This
closes the single-host normal READ/CQE/owner-loop baseline. Cross-node,
permission/error, revoke race, peer exit, partial-post, and Dragonfly production
integration remain open.

### First B7 run correction

The first parent run failed in `urma_register_seg()` with the shim fallback
status `-5`; the child EOF was only a consequence of that parent exit. UDMA
accepts the READ-only/plain-token flags, while `urma_perftest` allocates every
registered buffer with page alignment. The initial probe passed an ordinary
Rust `Vec` heap address to `ummu_grant()`. The source wrapper now copies the
payload into shim-owned 4 KiB-aligned memory, registers that allocation, and
frees it only after peer drain/unimport acknowledgement, unregister, and token
release. The rerun described above passed.

## B7 command

Build:

```bash
cd /path/to/urma-transport-lab
cargo build --release --features urma --bin rm_read_probe
```

Parent shell:

```bash
./target/release/rm_read_probe \
  parent udmac0d1e2 1 127.0.0.1:31912 \
  67108864 1048576 128
```

Child shell:

```bash
./target/release/rm_read_probe \
  child udmac0d1e2 1 127.0.0.1:31912 \
  67108864 1048576 128
```

For this run, the child must report 64 completions,
`busyUnimportStatus: -16`, and `contentVerified`, `ownersRetired`, and
`cleanShutdown` as `true`. Preserve all observed CQE value sets in the B7
ledger before enabling production READ.
