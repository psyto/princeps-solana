# princeps-solana

**Compliance-gated building blocks for Solana, one program at a time:** a KYC transfer hook for
Token-2022, an asset vault, atomic delivery-vs-payment, repo and term lending — native
[Pinocchio](https://github.com/anza-xyz/pinocchio) programs (no Anchor) where the byte layout is
the contract. Take the one you need; the prime-broker flow they add up to is at the end.

## Pick a part

| I want to… | Part | What else it needs |
|---|---|---|
| refuse Token-2022 transfers unless both wallets are KYC-verified | `psol-compliance` (as a transfer hook) | — |
| keep a per-wallet registry of KYC status, jurisdiction and accreditation | `psol-compliance` | — |
| swap an asset for cash atomically, only if both parties pass KYC | `psol-settlement` | a registry¹ |
| hold one asset for many depositors, KYC-gating deposits and withdrawals | `psol-vault` | a registry¹ |
| lock two parties' vault positions for a repo | `psol-repo` | `psol-vault` (and its registry) |
| open a term loan against haircut, oracle-priced collateral | `psol-lending` | `psol-vault`, `psol-compliance` (risk params and price feeds) |
| read or write these accounts from your own program or client | `psol-types` (Rust crate) | `bytemuck` |

¹ A registry is any program that owns accounts in the `AccountRecord` layout — `psol-compliance`,
or your own. Each vault and each DvP ticket names its registry when it is created and accepts
records from no other program.

The vault, settlement, repo and lending programs take **plain Token-2022 mints only** — no
extensions. `init_vault` (`0x300F`) and `create_swap` (`0x2019`, both legs) refuse any other mint: a transfer fee
would leave less in the vault or escrow than was credited, and their token transfers do not
forward a transfer hook's extra accounts. So a mint whose transfer hook is `psol-compliance`
cannot go through them today; they gate on KYC with their own record checks instead.

## Quickstart

Each quickstart loads only the programs it uses into [LiteSVM](https://github.com/LiteSVM/litesvm),
no validator. Build the programs once (needs the Solana toolchain for `cargo build-sbf`):

```bash
git clone https://github.com/psyto/princeps-solana && cd princeps-solana
./scripts/build-sbf.sh
cargo run -p psol-quickstart --example kyc_transfer   # psol-compliance
cargo run -p psol-quickstart --example dvp            # psol-settlement
cargo run -p psol-quickstart --example vault          # psol-vault
cargo run -p psol-quickstart --example loan           # psol-lending on psol-vault
```
```
$ cargo run -p psol-quickstart --example kyc_transfer
both VERIFIED       transfer 100 -> ok
bob SUSPENDED       transfer 100 -> refused 0x1002  (TRANSFER_DENIED_SUSPENDED)
balances            alice 900, bob 100
$ cargo run -p psol-quickstart --example dvp
both legs funded    1,000 bond and 980 cash in escrow
buyer SUSPENDED     settle -> refused 0x2013  (COMPLIANCE_SUSPENDED)
buyer VERIFIED      settle -> ok
after               seller: 980 cash, buyer: 1000 bond
$ cargo run -p psol-quickstart --example vault
deposit 600         position 600 deposited
fund SUSPENDED      withdraw 200 -> refused 0x3013  (COMPLIANCE_SUSPENDED)
fund VERIFIED       withdraw 200 -> ok
after               position 400 deposited, wallet 600
$ cargo run -p psol-quickstart --example loan
lock 100k, borrow 600k -> refused 0x5035  (MARGIN_INSUFFICIENT)
lock 300k, borrow 200k -> ok
after open              borrower bond locked 300000, borrower cash 200000
repay                  -> ok
after repay             borrower bond locked 0, lender cash 400000
```

`psol-types` alone needs no SBF build:

```
$ cargo run -p psol-types --example check_record
status 1  ->  Err(Unverified)
status 2  ->  Ok(())
status 3  ->  Err(Suspended)
```

## Use a part in your own project

These are on-chain programs, so "using one" means deploying it and calling it. There is no IDL
or generated client.

- **Deploy the program you need.** `cargo build-sbf` in its directory, then
  `solana program deploy target/deploy/psol_settlement.so` (or whichever). A program needs only
  the programs in its row of the table above.
- **Call it from your client.** Each instruction's account order and data layout is documented
  above its handler in `programs/<part>/src/lib.rs`. Rust builders for the common instructions —
  the ones the quickstarts use — are in
  [`quickstart/examples/common/mod.rs`](quickstart/examples/common/mod.rs); copy what you need.
- **Read the accounts.** Depend on the layouts crate and decode the bytes you fetch:

  ```toml
  [dependencies]
  psol-types = { git = "https://github.com/psyto/princeps-solana" }
  ```
  ```rust
  use psol_types::read_account_record;

  // `data` = the AccountRecord PDA's bytes, from RPC or an AccountInfo.
  let record = read_account_record(&data)?;   // CheckError::LayoutInvalid on bad bytes
  record.check_transfer_allowed()?;            // Unverified / Suspended / Blocked
  ```
- **CPI from your own program.** Build the instruction bytes from the handler docs, as
  `psol-repo` and `psol-lending` do into `psol-vault` (`cpi_lock_position` and friends). Do not
  link a psol program crate into another on-chain program as a Rust dependency: two Pinocchio
  programs in one SBF binary clash on the allocator and panic handler. Copy the discriminator
  bytes instead.
- **Bring your own registry.** Pass your program as the compliance program at `init_vault` or
  `create_swap`. The parts accept any account your program owns that decodes as an
  `AccountRecord` whose `participant` is the wallet in question; `psol-lending` also reads
  `RiskParams` and `PriceFeed` accounts from it, at the PDAs `psol-compliance` uses.

Nothing here is audited or deployed anywhere. Treat it as a reference implementation.

**0.2.0 changes account layouts.** `Vault` grew from 104 to 136 bytes and `SwapTicket` from 168
to 200, and the ticket's PDA seeds now include the compliance program. There is no in-place
migration: accounts created by 0.1.0 do not load under 0.2.0, so a 0.1.0 deployment must be
redeployed to a new program id, not upgraded.

## Which programs each part trusts

Every part checks that the programs it relies on are the ones it was set up with, not whatever
the caller passes:

- `psol-vault` stores its registry in `Vault::compliance_program` at `init_vault`; deposit,
  withdraw, lock and internal transfers refuse records owned by any other program (`0x300C`).
  An internal transfer checks the sender's record, and a withdrawal pays only into the
  depositor's own token account, so a barred depositor cannot move value out through a verified
  one. One consequence: a suspended borrower cannot repay a `psol-lending` loan (the repayment
  is an internal transfer); the lender liquidates after maturity instead.
- `psol-settlement` stores the registry in the ticket and in the ticket's PDA seeds, so the ticket
  address both parties expect commits to it; `settle` refuses any other (`0x2016`).
- `psol-repo` and `psol-lending` require the vaults passed in to be the ones recorded in the
  repo or loan and owned by the vault program they call, and `psol-lending` requires its
  compliance program to be the one both vaults use (`0x4005`/`0x5005`, `0x5006`). Its margin
  check counts only positions owned by that vault program.

Vault and settlement also check every token account they move tokens through: the vault's own
account must be held by the vault PDA for that mint (`0x300E`), and DvP escrows must be held by
the ticket and each destination by the party it pays (`0x2018`). `psol-lending`'s repay and
liquidation, and `psol-repo`'s close, act only on the positions the agreement locked and the
parties it names: repay pays into the lender's own position, liquidation seizes from the
borrower's position into the lender's (`0x5009`, `0x4009` for repo).

A repo has no default path yet. Before its expiry only the borrower can close it; after expiry
either party can, which unlocks both positions.

One thing this does not cover: anyone can call `init_vault` for a mint first and choose its
registry. Initialise your vaults right after deploying, and check `Vault::compliance_program`
before depositing into a vault you did not create.

## How the parts fit: a prime broker

Together the parts make a small prime broker. `psol-compliance` is the registry. Participants
deposit into one `psol-vault` per asset (plain Token-2022 mints; see the note under the table). Two
participants can then lock positions for a repo (`psol-repo`), open a term loan whose collateral
is valued with per-asset-class haircuts and oracle prices (`psol-lending`, reading `RiskParams`
and `PriceFeed`s from the registry), or swap an asset for cash outright (`psol-settlement`). The
loan quickstart walks through compliance → vault → lending end to end.

This is the Solana counterpart to [`princeps`](https://github.com/psyto/rdk/tree/main/princeps),
the prime broker on Reth in rdk, and a sibling to
[`openhl-solana`](https://github.com/psyto/openhl-solana), the perp DEX on Solana.

## Build and test

```bash
./scripts/build-sbf.sh       # every program to target/deploy/*.so (the e2e tests load these)
cargo test --workspace       # 156 tests: unit + LiteSVM end-to-end for every part
```

### Fuzzing the vault

[`fuzz/psol-vault`](fuzz/psol-vault) fuzzes `deposit` and `withdraw` against the built programs with
[solinv](https://github.com/psyto/solinv)'s signer-skip, owner-skip and account-swap checks. It
swaps in another depositor's record or position, a token account the vault does not hold, or
another compliance program. CI runs it for 3 minutes on every pull request that touches the vault,
the compliance program or `psol-types` (`.github/workflows/fuzz-vault.yml`), and fails on any finding.

Measured by deleting one check at a time: the vault-side account on deposit, the withdrawal
destination, the position binding and the record's participant are each caught; the current code
gives 0 findings. The registry pin on withdraw is not caught, because the attack needs two accounts
swapped together. The e2e test `withdraw_rejects_records_from_another_compliance_program` covers it.
Details: [`fuzz/psol-vault/README.md`](fuzz/psol-vault/README.md).

## License

MIT OR Apache-2.0.
