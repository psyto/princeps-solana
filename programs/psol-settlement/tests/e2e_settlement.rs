//! End-to-end tests for `psol-settlement` (self-contained atomic DvP).
//!
//! Walks through:
//!   1. Compliance bootstrap; two verified parties (a + b).
//!   2. Two plain Token-2022 mints (asset + cash); escrow ATAs owned by the
//!      ticket PDA; each party's source + destination ATAs.
//!   3. `create_swap` → `fund` (both legs) → `settle`; assert both legs
//!      delivered atomically and escrows drained.
//!   4. Negatives: settle before both funded rejects; settle with a
//!      SUSPENDED party rejects; expiry → `reclaim` refunds the funded leg.
//!
//! Prerequisite: `psol_compliance.so` + `psol_settlement.so` under
//! `target/deploy/` (`cargo build-sbf`). If absent, the tests SKIP.

use {
    litesvm::LiteSVM,
    solana_address::Address,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_message::Message,
    solana_program_pack::Pack,
    solana_signer::Signer,
    solana_transaction::Transaction,
    spl_token_2022::state::Mint,
    psol_types::{compliance_status, seeds},
    std::path::PathBuf,
};

// ─── Harness ─────────────────────────────────────────────────────────────

fn compliance_program_id() -> Address {
    Address::from([7u8; 32])
}
fn settlement_program_id() -> Address {
    Address::from([13u8; 32])
}
fn token_2022_id() -> Address {
    spl_token_2022::ID.to_bytes().into()
}
const ATA_PROGRAM_ID: Address = Address::new_from_array(pinocchio_pubkey::from_str(
    "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
));

fn so_path(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../target/deploy/");
    p.push(name);
    p
}

fn setup() -> Option<(LiteSVM, Keypair)> {
    for (label, path) in [
        ("psol_compliance.so", so_path("psol_compliance.so")),
        ("psol_settlement.so", so_path("psol_settlement.so")),
    ] {
        if !path.exists() {
            eprintln!("SKIP: {label} not built. Run `cargo build-sbf` first.");
            return None;
        }
    }
    let mut svm = LiteSVM::new();
    svm.add_program(
        compliance_program_id(),
        &std::fs::read(so_path("psol_compliance.so")).unwrap(),
    )
    .unwrap();
    svm.add_program(
        settlement_program_id(),
        &std::fs::read(so_path("psol_settlement.so")).unwrap(),
    )
    .unwrap();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 100_000_000_000).unwrap();
    Some((svm, payer))
}

fn send(svm: &mut LiteSVM, ixs: &[Instruction], signers: &[&Keypair]) {
    let blockhash = svm.latest_blockhash();
    let payer = signers[0].pubkey();
    let msg = Message::new(ixs, Some(&payer));
    let tx = Transaction::new(signers, msg, blockhash);
    svm.send_transaction(tx).expect("tx failed");
}
fn try_send(svm: &mut LiteSVM, ixs: &[Instruction], signers: &[&Keypair]) -> bool {
    let blockhash = svm.latest_blockhash();
    let payer = signers[0].pubkey();
    let msg = Message::new(ixs, Some(&payer));
    let tx = Transaction::new(signers, msg, blockhash);
    svm.send_transaction(tx).is_ok()
}

// ─── Derivations ─────────────────────────────────────────────────────────

fn derive_registry() -> Address {
    Address::find_program_address(&[seeds::REGISTRY], &compliance_program_id()).0
}
fn derive_record(participant: &Address) -> Address {
    Address::find_program_address(
        &[seeds::ACCOUNT_RECORD, participant.as_ref()],
        &compliance_program_id(),
    )
    .0
}
fn derive_ata(owner: &Address, mint: &Address) -> Address {
    Address::find_program_address(
        &[owner.as_ref(), token_2022_id().as_ref(), mint.as_ref()],
        &ATA_PROGRAM_ID,
    )
    .0
}
fn derive_ticket(a: &Address, b: &Address, ma: &Address, mb: &Address, nonce: u64) -> Address {
    let n = nonce.to_le_bytes();
    Address::find_program_address(
        &[
            psol_settlement::DVP,
            a.as_ref(),
            b.as_ref(),
            ma.as_ref(),
            mb.as_ref(),
            &n,
            compliance_program_id().as_ref(),
        ],
        &settlement_program_id(),
    )
    .0
}

// ─── Compliance instruction builders ─────────────────────────────────────

fn ix_initialize_registry(payer: &Address) -> Instruction {
    Instruction {
        program_id: compliance_program_id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(derive_registry(), false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
        ],
        data: vec![psol_compliance::ix::INITIALIZE_REGISTRY],
    }
}
fn ix_register_account(payer: &Address, participant: &Address) -> Instruction {
    let mut data = Vec::with_capacity(1 + 32 + 2);
    data.push(psol_compliance::ix::REGISTER_ACCOUNT);
    data.extend_from_slice(participant.as_ref());
    data.extend_from_slice(b"JP");
    Instruction {
        program_id: compliance_program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*payer, true),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(derive_registry(), false),
            AccountMeta::new(derive_record(participant), false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
        ],
        data,
    }
}
fn ix_update_status(payer: &Address, participant: &Address, status: u8) -> Instruction {
    Instruction {
        program_id: compliance_program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*payer, true),
            AccountMeta::new_readonly(derive_registry(), false),
            AccountMeta::new(derive_record(participant), false),
        ],
        data: vec![
            psol_compliance::ix::UPDATE_STATUS,
            status,
            0,
            psol_compliance::change_mask::STATUS,
        ],
    }
}
fn onboard(svm: &mut LiteSVM, payer: &Keypair, party: &Address) {
    send(svm, &[ix_register_account(&payer.pubkey(), party)], &[payer]);
    send(
        svm,
        &[ix_update_status(&payer.pubkey(), party, compliance_status::VERIFIED)],
        &[payer],
    );
}

// ─── Token-2022 helpers ──────────────────────────────────────────────────

fn create_plain_mint(svm: &mut LiteSVM, payer: &Keypair, mint_kp: &Keypair, authority: &Address, decimals: u8) {
    let space = Mint::LEN;
    let lamports = svm.minimum_balance_for_rent_exemption(space);
    let create = solana_system_interface::instruction::create_account(
        &payer.pubkey(),
        &mint_kp.pubkey(),
        lamports,
        space as u64,
        &spl_token_2022::ID,
    );
    let init = spl_token_2022::instruction::initialize_mint2(
        &spl_token_2022::ID,
        &mint_kp.pubkey(),
        authority,
        None,
        decimals,
    )
    .unwrap();
    send(svm, &[create, init], &[payer, mint_kp]);
}
fn create_ata(svm: &mut LiteSVM, payer: &Keypair, mint: &Address, owner: &Address) -> Address {
    let ata = derive_ata(owner, mint);
    let ix = Instruction {
        program_id: ATA_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(ata, false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new_readonly(token_2022_id(), false),
        ],
        data: vec![1u8],
    };
    send(svm, &[ix], &[payer]);
    ata
}
fn mint_to(svm: &mut LiteSVM, payer: &Keypair, mint: &Address, authority: &Keypair, dest: &Address, amount: u64) {
    let ix = spl_token_2022::instruction::mint_to(
        &spl_token_2022::ID,
        mint,
        dest,
        &authority.pubkey(),
        &[],
        amount,
    )
    .unwrap();
    send(svm, &[ix], &[payer, authority]);
}
fn balance(svm: &LiteSVM, ata: &Address) -> u64 {
    let acc = svm.get_account(ata).expect("ata missing");
    u64::from_le_bytes(acc.data[64..72].try_into().unwrap())
}

// ─── Settlement instruction builders ─────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn ix_create_swap(
    payer: &Address,
    ticket: &Address,
    a: &Address,
    b: &Address,
    ma: &Address,
    mb: &Address,
    amount_a: u64,
    amount_b: u64,
    decimals: u8,
    expiry_slot: u64,
    nonce: u64,
) -> Instruction {
    let mut data = Vec::with_capacity(1 + 162);
    data.push(psol_settlement::ix::CREATE_SWAP);
    data.extend_from_slice(a.as_ref());
    data.extend_from_slice(b.as_ref());
    data.extend_from_slice(ma.as_ref());
    data.extend_from_slice(mb.as_ref());
    data.extend_from_slice(&amount_a.to_le_bytes());
    data.extend_from_slice(&amount_b.to_le_bytes());
    data.push(decimals);
    data.push(decimals);
    data.extend_from_slice(&expiry_slot.to_le_bytes());
    data.extend_from_slice(&nonce.to_le_bytes());
    Instruction {
        program_id: settlement_program_id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(*ticket, false),
            AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            AccountMeta::new_readonly(compliance_program_id(), false),
        ],
        data,
    }
}
fn ix_fund(funder: &Address, ticket: &Address, source: &Address, escrow: &Address, mint: &Address) -> Instruction {
    Instruction {
        program_id: settlement_program_id(),
        accounts: vec![
            AccountMeta::new_readonly(*funder, true),
            AccountMeta::new(*ticket, false),
            AccountMeta::new(*source, false),
            AccountMeta::new(*escrow, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(token_2022_id(), false),
        ],
        data: vec![psol_settlement::ix::FUND],
    }
}
#[allow(clippy::too_many_arguments)]
fn ix_settle(
    ticket: &Address,
    escrow_a: &Address,
    escrow_b: &Address,
    a_dst: &Address,
    b_dst: &Address,
    ma: &Address,
    mb: &Address,
    record_a: &Address,
    record_b: &Address,
) -> Instruction {
    Instruction {
        program_id: settlement_program_id(),
        accounts: vec![
            AccountMeta::new(*ticket, false),
            AccountMeta::new(*escrow_a, false),
            AccountMeta::new(*escrow_b, false),
            AccountMeta::new(*a_dst, false),
            AccountMeta::new(*b_dst, false),
            AccountMeta::new_readonly(*ma, false),
            AccountMeta::new_readonly(*mb, false),
            AccountMeta::new_readonly(*record_a, false),
            AccountMeta::new_readonly(*record_b, false),
            AccountMeta::new_readonly(compliance_program_id(), false),
            AccountMeta::new_readonly(token_2022_id(), false),
        ],
        data: vec![psol_settlement::ix::SETTLE],
    }
}
#[allow(clippy::too_many_arguments)]
fn ix_reclaim(
    ticket: &Address,
    escrow_a: &Address,
    escrow_b: &Address,
    a_dst: &Address,
    b_dst: &Address,
    ma: &Address,
    mb: &Address,
) -> Instruction {
    Instruction {
        program_id: settlement_program_id(),
        accounts: vec![
            AccountMeta::new(*ticket, false),
            AccountMeta::new(*escrow_a, false),
            AccountMeta::new(*escrow_b, false),
            AccountMeta::new(*a_dst, false),
            AccountMeta::new(*b_dst, false),
            AccountMeta::new_readonly(*ma, false),
            AccountMeta::new_readonly(*mb, false),
            AccountMeta::new_readonly(token_2022_id(), false),
        ],
        data: vec![psol_settlement::ix::RECLAIM],
    }
}

/// Full fixture: verified parties, two mints, escrow + party ATAs, funded
/// sources. Returns everything a settle/reclaim call needs.
struct Fixture {
    party_a: Keypair,
    party_b: Keypair,
    mint_a: Address,
    mint_b: Address,
    ticket: Address,
    escrow_a: Address,
    escrow_b: Address,
    a_src: Address,
    b_src: Address,
    a_dst: Address,
    b_dst: Address,
    amount_a: u64,
    amount_b: u64,
}

const DECIMALS: u8 = 6;

fn build_fixture(svm: &mut LiteSVM, payer: &Keypair, expiry_slot: u64) -> Fixture {
    send(svm, &[ix_initialize_registry(&payer.pubkey())], &[payer]);
    let party_a = Keypair::new();
    let party_b = Keypair::new();
    svm.airdrop(&party_a.pubkey(), 1_000_000_000).unwrap();
    svm.airdrop(&party_b.pubkey(), 1_000_000_000).unwrap();
    onboard(svm, payer, &party_a.pubkey());
    onboard(svm, payer, &party_b.pubkey());

    let mint_a_kp = Keypair::new();
    let mint_b_kp = Keypair::new();
    create_plain_mint(svm, payer, &mint_a_kp, &payer.pubkey(), DECIMALS);
    create_plain_mint(svm, payer, &mint_b_kp, &payer.pubkey(), DECIMALS);
    let mint_a = mint_a_kp.pubkey();
    let mint_b = mint_b_kp.pubkey();

    let (amount_a, amount_b) = (1_000u64, 2_000u64);
    let nonce = 42u64;
    let ticket = derive_ticket(&party_a.pubkey(), &party_b.pubkey(), &mint_a, &mint_b, nonce);

    // Escrow ATAs owned by the ticket PDA.
    let escrow_a = create_ata(svm, payer, &mint_a, &ticket);
    let escrow_b = create_ata(svm, payer, &mint_b, &ticket);
    // Source ATAs (funded) + destination ATAs.
    let a_src = create_ata(svm, payer, &mint_a, &party_a.pubkey());
    let b_src = create_ata(svm, payer, &mint_b, &party_b.pubkey());
    let a_dst = create_ata(svm, payer, &mint_b, &party_a.pubkey()); // party_a receives cash
    let b_dst = create_ata(svm, payer, &mint_a, &party_b.pubkey()); // party_b receives asset
    mint_to(svm, payer, &mint_a, payer, &a_src, amount_a);
    mint_to(svm, payer, &mint_b, payer, &b_src, amount_b);

    send(
        svm,
        &[ix_create_swap(
            &payer.pubkey(),
            &ticket,
            &party_a.pubkey(),
            &party_b.pubkey(),
            &mint_a,
            &mint_b,
            amount_a,
            amount_b,
            DECIMALS,
            expiry_slot,
            nonce,
        )],
        &[payer],
    );

    Fixture {
        party_a,
        party_b,
        mint_a,
        mint_b,
        ticket,
        escrow_a,
        escrow_b,
        a_src,
        b_src,
        a_dst,
        b_dst,
        amount_a,
        amount_b,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[test]
fn dvp_happy_path_settles_both_legs_atomically() {
    let Some((mut svm, payer)) = setup() else { return };
    let fx = build_fixture(&mut svm, &payer, u64::MAX);

    send(&mut svm, &[ix_fund(&fx.party_a.pubkey(), &fx.ticket, &fx.a_src, &fx.escrow_a, &fx.mint_a)], &[&payer, &fx.party_a]);
    send(&mut svm, &[ix_fund(&fx.party_b.pubkey(), &fx.ticket, &fx.b_src, &fx.escrow_b, &fx.mint_b)], &[&payer, &fx.party_b]);
    assert_eq!(balance(&svm, &fx.escrow_a), fx.amount_a);
    assert_eq!(balance(&svm, &fx.escrow_b), fx.amount_b);

    send(
        &mut svm,
        &[ix_settle(
            &fx.ticket, &fx.escrow_a, &fx.escrow_b, &fx.a_dst, &fx.b_dst,
            &fx.mint_a, &fx.mint_b,
            &derive_record(&fx.party_a.pubkey()), &derive_record(&fx.party_b.pubkey()),
        )],
        &[&payer],
    );

    // Asset leg (mint_a) → party_b; cash leg (mint_b) → party_a; escrows drained.
    assert_eq!(balance(&svm, &fx.b_dst), fx.amount_a);
    assert_eq!(balance(&svm, &fx.a_dst), fx.amount_b);
    assert_eq!(balance(&svm, &fx.escrow_a), 0);
    assert_eq!(balance(&svm, &fx.escrow_b), 0);

    // Double-settle rejects.
    assert!(!try_send(
        &mut svm,
        &[ix_settle(
            &fx.ticket, &fx.escrow_a, &fx.escrow_b, &fx.a_dst, &fx.b_dst,
            &fx.mint_a, &fx.mint_b,
            &derive_record(&fx.party_a.pubkey()), &derive_record(&fx.party_b.pubkey()),
        )],
        &[&payer],
    ));
}

#[test]
fn settle_rejects_when_not_fully_funded() {
    let Some((mut svm, payer)) = setup() else { return };
    let fx = build_fixture(&mut svm, &payer, u64::MAX);

    // Only party_a funds.
    send(&mut svm, &[ix_fund(&fx.party_a.pubkey(), &fx.ticket, &fx.a_src, &fx.escrow_a, &fx.mint_a)], &[&payer, &fx.party_a]);
    assert!(!try_send(
        &mut svm,
        &[ix_settle(
            &fx.ticket, &fx.escrow_a, &fx.escrow_b, &fx.a_dst, &fx.b_dst,
            &fx.mint_a, &fx.mint_b,
            &derive_record(&fx.party_a.pubkey()), &derive_record(&fx.party_b.pubkey()),
        )],
        &[&payer],
    ));
}

#[test]
fn settle_rejects_suspended_party() {
    let Some((mut svm, payer)) = setup() else { return };
    let fx = build_fixture(&mut svm, &payer, u64::MAX);
    send(&mut svm, &[ix_fund(&fx.party_a.pubkey(), &fx.ticket, &fx.a_src, &fx.escrow_a, &fx.mint_a)], &[&payer, &fx.party_a]);
    send(&mut svm, &[ix_fund(&fx.party_b.pubkey(), &fx.ticket, &fx.b_src, &fx.escrow_b, &fx.mint_b)], &[&payer, &fx.party_b]);

    // Suspend party_a after funding.
    send(&mut svm, &[ix_update_status(&payer.pubkey(), &fx.party_a.pubkey(), compliance_status::SUSPENDED)], &[&payer]);
    assert!(!try_send(
        &mut svm,
        &[ix_settle(
            &fx.ticket, &fx.escrow_a, &fx.escrow_b, &fx.a_dst, &fx.b_dst,
            &fx.mint_a, &fx.mint_b,
            &derive_record(&fx.party_a.pubkey()), &derive_record(&fx.party_b.pubkey()),
        )],
        &[&payer],
    ));
}

#[test]
fn reclaim_refunds_funded_leg_after_expiry() {
    let Some((mut svm, payer)) = setup() else { return };
    // expiry_slot = 0 → already expired (current slot > 0).
    let fx = build_fixture(&mut svm, &payer, 0);
    send(&mut svm, &[ix_fund(&fx.party_a.pubkey(), &fx.ticket, &fx.a_src, &fx.escrow_a, &fx.mint_a)], &[&payer, &fx.party_a]);
    assert_eq!(balance(&svm, &fx.escrow_a), fx.amount_a);

    // Advance past the expiry slot so `reclaim` is permitted.
    svm.warp_to_slot(10);

    // party_a's asset-leg refund lands in its mint_a ATA (a_src).
    send(
        &mut svm,
        &[ix_reclaim(&fx.ticket, &fx.escrow_a, &fx.escrow_b, &fx.a_src, &fx.b_src, &fx.mint_a, &fx.mint_b)],
        &[&payer],
    );
    assert_eq!(balance(&svm, &fx.escrow_a), 0);
    assert_eq!(balance(&svm, &fx.a_src), fx.amount_a);
}

/// A suspended party deploys their own copy of the compliance program,
/// marks both parties VERIFIED there, and settles with those records. The
/// ticket was created against the real registry, so `settle` must refuse.
#[test]
fn settle_rejects_records_from_another_compliance_program() {
    let Some((mut svm, payer)) = setup() else { return };
    let rogue = Address::from([8u8; 32]);
    svm.add_program(rogue, &std::fs::read(so_path("psol_compliance.so")).unwrap())
        .unwrap();
    let fx = build_fixture(&mut svm, &payer, u64::MAX);
    send(&mut svm, &[ix_fund(&fx.party_a.pubkey(), &fx.ticket, &fx.a_src, &fx.escrow_a, &fx.mint_a)], &[&payer, &fx.party_a]);
    send(&mut svm, &[ix_fund(&fx.party_b.pubkey(), &fx.ticket, &fx.b_src, &fx.escrow_b, &fx.mint_b)], &[&payer, &fx.party_b]);
    send(&mut svm, &[ix_update_status(&payer.pubkey(), &fx.party_a.pubkey(), compliance_status::SUSPENDED)], &[&payer]);

    // party_a's own registry, where both parties are VERIFIED.
    let a = fx.party_a.pubkey();
    let rogue_registry = Address::find_program_address(&[seeds::REGISTRY], &rogue).0;
    let rogue_record =
        |p: &Address| Address::find_program_address(&[seeds::ACCOUNT_RECORD, p.as_ref()], &rogue).0;
    let retarget = |mut ix: Instruction, p: Option<&Address>| {
        ix.program_id = rogue;
        for m in &mut ix.accounts {
            if m.pubkey == derive_registry() {
                m.pubkey = rogue_registry;
            } else if let Some(p) = p {
                if m.pubkey == derive_record(p) {
                    m.pubkey = rogue_record(p);
                }
            }
        }
        ix
    };
    let mut ixs = vec![retarget(ix_initialize_registry(&a), None)];
    for p in [fx.party_a.pubkey(), fx.party_b.pubkey()] {
        ixs.push(retarget(ix_register_account(&a, &p), Some(&p)));
        ixs.push(retarget(ix_update_status(&a, &p, compliance_status::VERIFIED), Some(&p)));
    }
    send(&mut svm, &ixs, &[&fx.party_a]);

    let mut ix = ix_settle(
        &fx.ticket, &fx.escrow_a, &fx.escrow_b, &fx.a_dst, &fx.b_dst,
        &fx.mint_a, &fx.mint_b,
        &rogue_record(&fx.party_a.pubkey()), &rogue_record(&fx.party_b.pubkey()),
    );
    ix.accounts[9].pubkey = rogue;
    assert!(!try_send(&mut svm, &[ix], &[&payer]), "settle must reject another program's records");
    assert_eq!(balance(&svm, &fx.escrow_a), fx.amount_a, "asset leg stays in escrow");
    assert_eq!(balance(&svm, &fx.escrow_b), fx.amount_b, "cash leg stays in escrow");
}

/// After both legs are funded, a third party calls `settle` with their own
/// token accounts as both destinations. `settle` must refuse: each leg
/// goes only to the counterparty's own account.
#[test]
fn settle_rejects_destinations_not_owned_by_the_parties() {
    let Some((mut svm, payer)) = setup() else { return };
    let fx = build_fixture(&mut svm, &payer, u64::MAX);
    send(&mut svm, &[ix_fund(&fx.party_a.pubkey(), &fx.ticket, &fx.a_src, &fx.escrow_a, &fx.mint_a)], &[&payer, &fx.party_a]);
    send(&mut svm, &[ix_fund(&fx.party_b.pubkey(), &fx.ticket, &fx.b_src, &fx.escrow_b, &fx.mint_b)], &[&payer, &fx.party_b]);

    let thief = Keypair::new();
    let thief_cash = create_ata(&mut svm, &payer, &fx.mint_b, &thief.pubkey());
    let thief_asset = create_ata(&mut svm, &payer, &fx.mint_a, &thief.pubkey());
    assert!(!try_send(
        &mut svm,
        &[ix_settle(
            &fx.ticket, &fx.escrow_a, &fx.escrow_b, &thief_cash, &thief_asset,
            &fx.mint_a, &fx.mint_b,
            &derive_record(&fx.party_a.pubkey()), &derive_record(&fx.party_b.pubkey()),
        )],
        &[&payer],
    ));
    assert_eq!(balance(&svm, &fx.escrow_a), fx.amount_a);
    assert_eq!(balance(&svm, &fx.escrow_b), fx.amount_b);
}
