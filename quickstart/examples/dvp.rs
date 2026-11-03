//! psol-settlement: swap an asset leg for a cash leg atomically. Neither
//! leg moves unless both are funded and both parties pass compliance.
//!
//!     ./scripts/build-sbf.sh && cargo run -p psol-quickstart --example dvp

mod common;
use {
    common::*,
    psol_types::compliance_status::{SUSPENDED, VERIFIED},
    solana_signer::Signer,
};

fn main() {
    let mut env = env(&[("psol_compliance", COMPLIANCE), ("psol_settlement", SETTLEMENT)]);
    let admin = env.admin.pubkey();
    env.ok(&[init_registry(&admin)], &[]);

    let (seller, buyer) = (env.user(), env.user());
    onboard(&mut env, &seller.pubkey());
    onboard(&mut env, &buyer.pubkey());

    let (bond, cash) = (plain_mint(&mut env, 6), plain_mint(&mut env, 6));
    let seller_bond = ata(&mut env, &bond, &seller.pubkey());
    let buyer_cash = ata(&mut env, &cash, &buyer.pubkey());
    let seller_cash = ata(&mut env, &cash, &seller.pubkey());
    let buyer_bond = ata(&mut env, &bond, &buyer.pubkey());
    mint_to(&mut env, &bond, &seller_bond, 1_000);
    mint_to(&mut env, &cash, &buyer_cash, 980);

    let swap = Swap {
        a: seller.pubkey(),
        b: buyer.pubkey(),
        mint_a: bond,
        mint_b: cash,
        amount_a: 1_000,
        amount_b: 980,
        decimals: 6,
        nonce: 1,
    };
    swap.create(&mut env, u64::MAX);
    env.ok(&[swap.fund(&seller.pubkey())], &[&seller]);
    env.ok(&[swap.fund(&buyer.pubkey())], &[&buyer]);
    println!("both legs funded    1,000 bond and 980 cash in escrow");

    env.ok(&[set_status(&admin, &buyer.pubkey(), SUSPENDED)], &[]);
    println!("buyer SUSPENDED     settle -> {:?}  (COMPLIANCE_SUSPENDED)", env.attempt(&[swap.settle()], &[]));

    env.ok(&[set_status(&admin, &buyer.pubkey(), VERIFIED)], &[]);
    println!("buyer VERIFIED      settle -> {:?}", env.attempt(&[swap.settle()], &[]));
    println!(
        "after               seller: {} cash, buyer: {} bond",
        env.balance(&seller_cash),
        env.balance(&buyer_bond)
    );
}
