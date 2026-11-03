//! psol-lending on psol-vault: a bilateral term loan. Opening it locks
//! the borrower's collateral and moves the lender's cash, but only if the
//! borrower's haircut, oracle-priced collateral covers the loan.
//!
//!     ./scripts/build-sbf.sh && cargo run -p psol-quickstart --example loan

mod common;
use {
    common::*,
    psol_types::asset_class::{SOVEREIGN_BOND, STABLECOIN},
    solana_signer::Signer,
};

fn main() {
    let mut env = env(&[
        ("psol_compliance", COMPLIANCE),
        ("psol_vault", VAULT),
        ("psol_lending", LENDING),
    ]);
    let admin = env.admin.pubkey();
    env.ok(&[init_registry(&admin), init_risk_params(&admin)], &[]);

    let (borrower, lender) = (env.user(), env.user());
    let (b, l) = (borrower.pubkey(), lender.pubkey());
    onboard(&mut env, &b);
    onboard(&mut env, &l);

    // Bond collateral (5% haircut) and stablecoin cash, both priced at $1.
    let (bond, usd) = (plain_mint(&mut env, 6), plain_mint(&mut env, 6));
    let bond_vault = init_vault(&mut env, &bond, SOVEREIGN_BOND);
    let usd_vault = init_vault(&mut env, &usd, STABLECOIN);
    env.ok(
        &[register_price(&admin, &bond, 1_000_000, 6), register_price(&admin, &usd, 1_000_000, 6)],
        &[],
    );

    let w = ata(&mut env, &bond, &b);
    mint_to(&mut env, &bond, &w, 600_000);
    let w = ata(&mut env, &usd, &l);
    mint_to(&mut env, &usd, &w, 400_000);
    env.ok(&[deposit(&admin, &b, &bond, 600_000)], &[&borrower]);
    env.ok(&[deposit(&admin, &l, &usd, 400_000)], &[&lender]);
    env.ok(&[init_position(&admin, &b, &usd)], &[&borrower]);

    let open = |env: &mut Env, collateral, principal, nonce| {
        env.attempt(&[open_loan(&admin, &b, &l, &bond, &usd, collateral, principal, nonce)], &[&borrower, &lender])
    };
    println!("lock 100k, borrow 600k -> {:?}  (MARGIN_INSUFFICIENT)", open(&mut env, 100_000, 600_000, 1));
    println!("lock 300k, borrow 200k -> {:?}", open(&mut env, 300_000, 200_000, 2));
    println!(
        "after open              borrower bond locked {}, borrower cash {}",
        read_position(&env, &bond_vault, &b).locked_amount,
        read_position(&env, &usd_vault, &b).amount_deposited
    );

    println!("repay                  -> {:?}", env.attempt(&[repay_loan(&b, &l, &bond, &usd, 2)], &[&borrower]));
    println!(
        "after repay             borrower bond locked {}, lender cash {}",
        read_position(&env, &bond_vault, &b).locked_amount,
        read_position(&env, &usd_vault, &l).amount_deposited
    );
}
