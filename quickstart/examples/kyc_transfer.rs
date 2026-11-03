//! psol-compliance as a Token-2022 transfer hook: every transfer of the
//! mint is checked against both parties' records.
//!
//!     ./scripts/build-sbf.sh && cargo run -p psol-quickstart --example kyc_transfer

mod common;
use {common::*, psol_types::compliance_status::SUSPENDED, solana_signer::Signer};

fn main() {
    let mut env = env(&[("psol_compliance", COMPLIANCE)]);
    let admin = env.admin.pubkey();
    env.ok(&[init_registry(&admin)], &[]);

    let (alice, bob) = (env.user(), env.user());
    onboard(&mut env, &alice.pubkey());
    onboard(&mut env, &bob.pubkey());

    let mint = hooked_mint(&mut env, 6);
    let alice_acct = hooked_token_account(&mut env, &mint, &alice.pubkey());
    let bob_acct = hooked_token_account(&mut env, &mint, &bob.pubkey());
    mint_to(&mut env, &mint, &alice_acct, 1_000);

    let send = |env: &mut Env, amount| {
        let ix = hooked_transfer(&alice_acct, &alice.pubkey(), &bob_acct, &bob.pubkey(), &mint, amount, 6);
        env.attempt(&[ix], &[&alice])
    };

    println!("both VERIFIED       transfer 100 -> {:?}", send(&mut env, 100));
    env.ok(&[set_status(&admin, &bob.pubkey(), SUSPENDED)], &[]);
    println!("bob SUSPENDED       transfer 100 -> {:?}  (TRANSFER_DENIED_SUSPENDED)", send(&mut env, 100));
    println!("balances            alice {}, bob {}", env.balance(&alice_acct), env.balance(&bob_acct));
}
