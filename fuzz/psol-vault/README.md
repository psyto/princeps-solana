# psol-vault fuzz harness (solinv)

Fuzzes `deposit` and `withdraw` against the real `psol_compliance.so`
and `psol_vault.so` with [solinv](https://github.com/psyto/solinv)'s
signer-skip, owner-skip and account-swap checks. CI runs it on every
pull request that touches the vault (`.github/workflows/fuzz-vault.yml`).

```bash
./scripts/build-sbf.sh                       # from the repo root
cd fuzz/psol-vault
cargo build --release --features invariant_vault
timeout -s INT 180 crucible run psol_vault_fuzz invariant_vault --binary-in target/release/invariant_test --timeout 180
```

Any `[FUZZ_FINDING]` line is a failure (crucible itself exits 0). The outer
`timeout` is there because crucible checks `--timeout` only every 1000
iterations, about 4 minutes locally and 19 on `ubuntu-latest` for this
harness ([crucible#25](https://github.com/asymmetric-research/crucible/issues/25)). On macOS, `timeout` is `gtimeout` from coreutils.

## What it catches

Setup verifies two depositors, A and B, each with a position, and writes
a copy of A's record owned by a rogue program. Each spec sends A's
instruction with one account swapped; two swap sets (solinv's
`account_swap::check_sets`) swap the compliance program and the record
together, in `deposit` and in `withdraw`. Measured 2026-11-04 by
deleting one check from `programs/psol-vault/src/lib.rs` at a time
(45-second runs):

| Check deleted | Caught |
|---|---|
| deposit: the vault-side token account must be held by the vault | yes, `deposit` slot 8 |
| withdraw: the destination must be the depositor's own account | yes, `withdraw` slot 7 |
| withdraw: the position must be (vault, depositor)'s | yes, `withdraw` slot 4 |
| `verify_record`: the record must be the signer's | yes, `deposit` slot 2 |
| withdraw: the compliance program must be the vault's | yes, `withdraw` slots 1 and 2 together (60-second run, solinv 8bcb011) |
| deposit: the compliance program must be the vault's | yes, `deposit` slots 2 and 3 together (60-second run, solinv 8bcb011) |
| none (current code) | 0 findings |

The fake-registry attack needs two accounts swapped together, another
compliance program *and* a record it owns: swapping either alone fails
on the other check, so one-slot swaps missed it until solinv added swap
sets ([psyto/solinv#4](https://github.com/psyto/solinv/issues/4)). The
e2e test `withdraw_rejects_records_from_another_compliance_program`
covers it too.

Not caught: a suspended sender in `transfer_within_vault` is a different
signer, not a swapped account; `transfer_within_vault_rejects_a_suspended_sender`
covers it.
