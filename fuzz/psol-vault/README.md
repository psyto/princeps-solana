# psol-vault fuzz harness (solinv)

Fuzzes `deposit` and `withdraw` against the real `psol_compliance.so`
and `psol_vault.so` with [solinv](https://github.com/psyto/solinv)'s
signer-skip, owner-skip and account-swap checks. CI runs it on every
pull request that touches the vault (`.github/workflows/fuzz-vault.yml`).

```bash
./scripts/build-sbf.sh                       # from the repo root
cd fuzz/psol-vault
cargo build --release --features invariant_vault
crucible run psol_vault_fuzz invariant_vault --binary-in target/release/invariant_test --timeout 60
```

Any `[FUZZ_FINDING]` line is a failure (crucible itself exits 0).

## What it catches

Setup verifies two depositors, A and B, each with a position. Each spec
sends A's instruction with one account swapped. Measured 2026-11-04 by
deleting one check from `programs/psol-vault/src/lib.rs` at a time
(45-second runs):

| Check deleted | Caught |
|---|---|
| deposit: the vault-side token account must be held by the vault | yes, `deposit` slot 8 |
| withdraw: the destination must be the depositor's own account | yes, `withdraw` slot 7 |
| withdraw: the position must be (vault, depositor)'s | yes, `withdraw` slot 4 |
| `verify_record`: the record must be the signer's | yes, `deposit` slot 2 |
| withdraw: the compliance program must be the vault's | **no** — see below |
| none (current code) | 0 findings |

**Not caught:** the fake-registry attack needs two accounts swapped
together, another compliance program *and* a record it owns. solinv's
account-swap changes one slot at a time, so swapping only the program
fails on the record check and looks safe. The e2e test
`withdraw_rejects_records_from_another_compliance_program` covers it.
Likewise a suspended sender in `transfer_within_vault` is a different
signer, not a swapped account; `transfer_within_vault_rejects_a_suspended_sender`
covers it.
