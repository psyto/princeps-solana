//! psol-vault: a per-asset vault whose deposits and withdrawals are
//! checked against the registry the vault was initialised with.
//!
//!     ./scripts/build-sbf.sh && cargo run -p psol-quickstart --example vault

mod common;
use {
    common::*,
    psol_types::{
        asset_class::SOVEREIGN_BOND,
        compliance_status::{SUSPENDED, VERIFIED},
    },
    solana_signer::Signer,
};

fn main() {
    let mut env = env(&[("psol_compliance", COMPLIANCE), ("psol_vault", VAULT)]);
    let admin = env.admin.pubkey();
    env.ok(&[init_registry(&admin)], &[]);

    let fund = env.user();
    let f = fund.pubkey();
    onboard(&mut env, &f);
    let bond = plain_mint(&mut env, 6);
    let wallet = ata(&mut env, &bond, &f);
    mint_to(&mut env, &bond, &wallet, 1_000);
    let v = init_vault(&mut env, &bond, SOVEREIGN_BOND);

    env.ok(&[deposit(&admin, &f, &bond, 600)], &[&fund]);
    println!("deposit 600         position {} deposited", read_position(&env, &v, &f).amount_deposited);

    env.ok(&[set_status(&admin, &f, SUSPENDED)], &[]);
    println!("fund SUSPENDED      withdraw 200 -> {:?}  (COMPLIANCE_SUSPENDED)", env.attempt(&[withdraw(&f, &bond, 200)], &[&fund]));

    env.ok(&[set_status(&admin, &f, VERIFIED)], &[]);
    println!("fund VERIFIED       withdraw 200 -> {:?}", env.attempt(&[withdraw(&f, &bond, 200)], &[&fund]));
    println!(
        "after               position {} deposited, wallet {}",
        read_position(&env, &v, &f).amount_deposited,
        env.balance(&wallet)
    );
}
