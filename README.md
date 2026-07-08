# princeps-solana

**Solana Prime Broker Sandbox engine.** Native Solana programs implementing prime-brokerage
primitives — compliance-gated transfers, collateral vaults, bilateral term lending, repo, and atomic
delivery-vs-payment settlement — built **without Anchor**, designed to make prime-broker behavior on
Solana explorable by running scenarios rather than reading code.

The Solana counterpart to [`princeps`](https://github.com/psyto/rdk) (the prime broker on Reth), and
a sibling to [`openhl-solana`](https://github.com/psyto/openhl-solana) (the perp DEX on Solana).

## What it is

A curated set of native (Pinocchio, `no_std`) Solana programs where the byte layout *is* the contract:

| program | what it does |
|---|---|
| `psol-compliance` | on-chain KYC/AML/accredited registry; a Token-2022 `TransferHook` gates every transfer against each party's `AccountRecord` |
| `psol-vault` | share-based collateral vault primitive |
| `psol-lending` | bilateral collateralized term loan, compliance-gated |
| `psol-repo` | bilateral time-bound lock (repurchase agreement), compliance-gated |
| `psol-settlement` | self-contained atomic delivery-vs-payment: `SwapTicket` PDA escrows both legs and settles them atomically only after both parties fund and pass compliance — no external settlement channel required |

Shared account layouts live in `crates/psol-types` (`repr(C)` + `bytemuck::Pod`).

## What you can explore

- **Compliance-gated transfers.** A Token-2022 mint whose `TransferHook` calls `psol-compliance` —
  observe how KYC/accreditation gating changes transfer semantics and error paths.
- **Cross-asset margin.** Collateral in mint A backs a loan in cash mint B; haircut-adjusted,
  oracle-priced. Tighten a haircut, watch free margin recompute.
- **Atomic DvP.** Two verified parties swap asset for cash atomically — with compliance enforced at
  the on-chain settlement layer, not off-chain.

## Build

```bash
cargo check
cargo test --workspace
```

## License

MIT OR Apache-2.0.
